//! 单步执行：一次「LLM ⇄ 工具」循环。
//!
//! 每步都是自包含的：全新消息列表、全新循环，与前序步骤只通过结果表交换数据。
//! 这是本模块唯一"自己写的循环" —— 为的是把 token / 耗时留在自己手里
//! （走 `chat` 的 `ChatService` 会在 driver 内部丢掉 `usage`）。

use std::sync::Arc;
use std::time::Instant;

use planned_agent_core::ai::types::{
    ChatCompletionRequest, Message, MessageContent, MessageRole, ToolDefinition,
};
use planned_agent_core::ai::AiClient;
use planned_agent_tool_manager::ToolRegistry;
use serde_json::Value;
use tokio::sync::watch;

use super::event::{PlanRunEvent, PlanRunSink};
use super::executor::ExecutorConfig;
use super::prompt;
use super::report::{CallUsage, StepRunRecord, StepStatus};
use super::template::PlanStep;

/// 输出摘要的字符上限。
const SUMMARY_MAX_CHARS: usize = 200;

/// 单步执行的输入。
pub(crate) struct StepInput<'a> {
    /// 步骤定义
    pub step: &'a PlanStep,
    /// 已展开占位符的 `intent`
    pub intent: &'a str,
    /// 依赖项的实际输出（`#En` → 文本），按依赖顺序
    pub prior: &'a [(String, String)],
    /// 暴露给 LLM 的工具定义
    pub tools: &'a [ToolDefinition],
    /// 步骤序号（从 1 开始）
    pub index: usize,
}

/// 单步执行结果。
pub(crate) struct StepRunResult {
    /// 报告用记录（同时进事件与最终报告）
    pub record: StepRunRecord,
    /// 完整输出（供下游步骤以 `#En` 引用）；失败为 `None`
    pub output: Option<String>,
}

/// 执行一个步骤。
///
/// 失败（LLM 报错 / 超轮数上限 / 用户取消）通过 `record.status` 与 `record.error`
/// 表达，**不向上抛错** —— 单步失败只终止这条流水线，不终止宿主。
pub(crate) async fn run_step(
    input: StepInput<'_>,
    ai: &Arc<dyn AiClient>,
    registry: &Arc<ToolRegistry>,
    cfg: &ExecutorConfig,
    sink: &dyn PlanRunSink,
    cancel: Option<&watch::Receiver<bool>>,
) -> StepRunResult {
    let started = Instant::now();
    let task = prompt::build_step_task(input.intent, &input.step.expected_output, input.prior);

    let mut messages = vec![
        text_message(MessageRole::System, prompt::STEP_SYSTEM_PROMPT),
        text_message(MessageRole::User, &task),
    ];

    let mut call_usages: Vec<CallUsage> = Vec::new();
    let mut tool_call_count = 0usize;
    let mut rounds = 0usize;
    let mut output: Option<String> = None;
    let mut error: Option<String> = None;

    loop {
        if is_cancelled(cancel) {
            error = Some("用户取消".to_string());
            break;
        }

        rounds += 1;
        let request = ChatCompletionRequest {
            model: ai.model_name().to_string(),
            messages: messages.clone(),
            tools: (!input.tools.is_empty()).then(|| input.tools.to_vec()),
            temperature: cfg.temperature,
            max_tokens: cfg.max_tokens,
            stream: false,
            extra: Default::default(),
        };

        let response = match ai.chat_completion(request).await {
            Ok(response) => response,
            Err(err) => {
                error = Some(format!("LLM 调用失败：{err}"));
                break;
            }
        };

        // token 采集：每次请求记一条（缺失记 0，保证 call_usages.len() == rounds）
        let (prompt_tokens, completion_tokens) = match response.usage {
            Some(usage) => (usage.prompt_tokens, usage.completion_tokens),
            None => (0, 0),
        };
        call_usages.push(CallUsage {
            round: rounds,
            prompt_tokens,
            completion_tokens,
        });

        let Some(choice) = response.choices.into_iter().next() else {
            error = Some("LLM 响应不含 choices".to_string());
            break;
        };
        let message = choice.message;

        let content = content_text(&message);
        let reasoning = message.reasoning_content.clone().unwrap_or_default();
        let thought = if !reasoning.is_empty() {
            reasoning.clone()
        } else {
            content.clone()
        };
        if !thought.is_empty() {
            sink.emit(PlanRunEvent::StepThought {
                index: input.index,
                round: rounds,
                text: thought,
            });
        }

        let tool_calls = message.tool_calls.clone().unwrap_or_default();
        if tool_calls.is_empty() {
            // 收敛：优先用回答正文，没有正文才退回思考内容
            let answer = if !content.is_empty() { content } else { reasoning };
            output = Some(answer);
            break;
        }

        // 已达轮数上限：不再执行工具，直接判失败（避免无界循环）
        if rounds >= cfg.max_rounds_per_step {
            error = Some(format!(
                "超过每步轮数上限 {}（末轮仍要求调用工具）",
                cfg.max_rounds_per_step
            ));
            break;
        }

        // 回灌 assistant 消息（含 tool_calls），再逐个执行工具
        messages.push(message);
        for call in tool_calls {
            if is_cancelled(cancel) {
                error = Some("用户取消".to_string());
                break;
            }

            let arguments = parse_arguments(&call.function.arguments);
            let (tool_output, is_error) =
                match registry.call_tool(&call.function.name, arguments).await {
                    Ok(outcome) => (
                        tool_content(&outcome.result.content),
                        outcome.result.is_error,
                    ),
                    Err(err) => (format!("工具执行失败：{err}"), true),
                };

            tool_call_count += 1;
            sink.emit(PlanRunEvent::StepToolCall {
                index: input.index,
                tool: call.function.name.clone(),
                ok: !is_error,
            });
            messages.push(tool_message(&call.id, &tool_output));
        }
        if error.is_some() {
            break;
        }
    }

    let prompt_tokens = call_usages.iter().map(|u| u.prompt_tokens).sum();
    let completion_tokens = call_usages.iter().map(|u| u.completion_tokens).sum();
    let status = if error.is_none() && output.is_some() {
        StepStatus::Done
    } else {
        StepStatus::Failed
    };
    let output_summary = output.as_deref().map(summarize);

    StepRunResult {
        record: StepRunRecord {
            index: input.index,
            result_reference: input.step.result_reference.clone(),
            intent: input.intent.to_string(),
            expected_output: input.step.expected_output.clone(),
            status,
            duration_ms: started.elapsed().as_millis() as u64,
            prompt_tokens,
            completion_tokens,
            tool_calls: tool_call_count,
            rounds,
            call_usages,
            output_summary,
            error,
        },
        output,
    }
}

/// 是否已收到取消信号。
fn is_cancelled(cancel: Option<&watch::Receiver<bool>>) -> bool {
    cancel.map(|receiver| *receiver.borrow()).unwrap_or(false)
}

/// 构造文本消息。
fn text_message(role: MessageRole, text: &str) -> Message {
    Message {
        role,
        content: Some(MessageContent::Text {
            text: text.to_string(),
        }),
        ..Default::default()
    }
}

/// 构造 tool 消息。
///
/// `MessageContent::ToolResult` 与顶层 `tool_call_id` 都要给：ai-openai 的
/// `convert_message` 要求后者非空，否则报 "Tool message must have tool_call_id"。
fn tool_message(tool_call_id: &str, content: &str) -> Message {
    Message {
        role: MessageRole::Tool,
        content: Some(MessageContent::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            content: content.to_string(),
        }),
        tool_call_id: Some(tool_call_id.to_string()),
        ..Default::default()
    }
}

/// 取消息的文本内容。
fn content_text(message: &Message) -> String {
    match &message.content {
        Some(MessageContent::Text { text }) => text.clone(),
        _ => String::new(),
    }
}

/// 解析工具调用的 `arguments`。
///
/// 非法 JSON 时退回原始字符串（宁可让工具自己报错，也不丢参数）。
fn parse_arguments(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// 工具输出转文本：字符串取原文，其余取 JSON 字面量。
fn tool_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// 生成输出摘要（超长截断）。
fn summarize(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= SUMMARY_MAX_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(SUMMARY_MAX_CHARS).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flexible::testing::{
        fake_tool, text_response, tool_response, FakeAiClient, RecordingSink,
    };
    use crate::flexible::{ExecutorConfig, StepStatus};
    use planned_agent_core::tool_registry::ToolCategory;
    use serde_json::json;

    const MAX_ROUNDS: usize = 5;

    fn cfg(max_rounds_per_step: usize) -> ExecutorConfig {
        ExecutorConfig {
            max_rounds_per_step,
            ..Default::default()
        }
    }

    /// 造一个只含单个步骤的输入，避免每条测试都写全字段。
    fn step(reference: &str) -> crate::flexible::PlanStep {
        crate::flexible::PlanStep {
            result_reference: reference.to_string(),
            intent: "做事".to_string(),
            expected_output: "做完".to_string(),
            dependencies: vec![],
        }
    }

    /// 注册一个假工具并返回 (registry, 调用记录)。
    fn registry_with(
        name: &str,
        output: Value,
        is_error: bool,
    ) -> (Arc<ToolRegistry>, Arc<super::super::testing::FakeTool>) {
        let registry = Arc::new(ToolRegistry::new());
        let tool = fake_tool(name, output, is_error);
        registry.register_custom_tool(
            planned_agent_core::mcp::types::Tool {
                name: name.to_string(),
                description: "测试工具".to_string(),
                input_schema: json!({"type": "object"}),
            },
            vec![ToolCategory::File],
            tool.clone(),
        );
        (registry, tool)
    }

    #[tokio::test]
    async fn converges_without_tool_calls() {
        let ai = FakeAiClient::new(vec![text_response("完成", 10, 5)]);
        let (registry, _tool) = registry_with("noop", json!("x"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            StepInput {
                step: &step_def,
                intent: "做事",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Done);
        assert_eq!(result.output.as_deref(), Some("完成"));
        assert_eq!(result.record.rounds, 1);
        assert_eq!(result.record.tool_calls, 0);
        // ⑤ 单次明细条数 == 轮数
        assert_eq!(result.record.call_usages.len(), result.record.rounds);
        // ⑥ 步骤级 token == 各轮之和
        assert_eq!(result.record.prompt_tokens, 10);
        assert_eq!(result.record.completion_tokens, 5);
    }

    #[tokio::test]
    async fn converges_after_one_tool_call() {
        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "read", json!({"path": "a.txt"}), 100, 20),
            text_response("已读取", 150, 10),
        ]);
        let (registry, tool) = registry_with("read", json!("file body"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            StepInput {
                step: &step_def,
                intent: "读文件",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Done);
        assert_eq!(result.output.as_deref(), Some("已读取"));
        assert_eq!(result.record.rounds, 2);
        assert_eq!(result.record.tool_calls, 1);
        assert_eq!(result.record.call_usages.len(), 2);
        // ⑥ 累加（prompt: 100 + 150）
        assert_eq!(result.record.prompt_tokens, 250);
        assert_eq!(result.record.completion_tokens, 30);

        // 工具确实被调用，且参数透传
        let calls = tool.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, json!({"path": "a.txt"}));

        // 事件流：StepToolCall(ok=true) 出现
        let events = sink.events();
        assert!(events.iter().any(|event| matches!(
            event,
            PlanRunEvent::StepToolCall { tool, ok: true, .. } if tool == "read"
        )));
    }

    #[tokio::test]
    async fn fails_when_exceeding_max_rounds() {
        // 每轮都要调工具 → 上限 2 时必然失败
        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "read", json!({}), 10, 1),
            tool_response("call-2", "read", json!({}), 10, 1),
        ]);
        let (registry, _tool) = registry_with("read", json!("body"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            StepInput {
                step: &step_def,
                intent: "读文件",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(2),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Failed);
        assert!(result.output.is_none());
        assert!(result
            .record
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("轮数上限"));
    }

    #[tokio::test]
    async fn error_tool_result_is_still_fed_back() {
        let ai = FakeAiClient::new(vec![
            tool_response("call-1", "read", json!({}), 10, 1),
            text_response("换个办法", 10, 1),
        ]);
        let (registry, _tool) = registry_with("read", json!("boom"), true);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            StepInput {
                step: &step_def,
                intent: "读文件",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        // 工具报错不终止本步：照常回灌后仍可收敛
        assert_eq!(result.record.status, StepStatus::Done);
        assert_eq!(result.record.tool_calls, 1);
        let events = sink.events();
        assert!(events.iter().any(|event| matches!(
            event,
            PlanRunEvent::StepToolCall { ok: false, .. }
        )));
    }

    #[tokio::test]
    async fn llm_error_marks_step_failed() {
        // 脚本为空 → FakeAiClient 返回 Err
        let ai = FakeAiClient::new(vec![]);
        let (registry, _tool) = registry_with("read", json!("body"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();

        let result = run_step(
            StepInput {
                step: &step_def,
                intent: "读文件",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            None,
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Failed);
        assert!(result
            .record
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("LLM 调用失败"));
    }

    /// 取消应在下一次检查点终止本步。
    #[tokio::test]
    async fn cancel_stops_before_first_call() {
        let ai = FakeAiClient::new(vec![text_response("不该被调用", 1, 1)]);
        let (registry, _tool) = registry_with("read", json!("body"), false);
        let step_def = step("#E1");
        let sink = RecordingSink::default();
        let (tx, rx) = watch::channel(true); // 一开始就是取消状态
        drop(tx);

        let result = run_step(
            StepInput {
                step: &step_def,
                intent: "读文件",
                prior: &[],
                tools: &[],
                index: 1,
            },
            &(ai as Arc<dyn AiClient>),
            &registry,
            &cfg(MAX_ROUNDS),
            &sink,
            Some(&rx),
        )
        .await;

        assert_eq!(result.record.status, StepStatus::Failed);
        assert_eq!(result.record.rounds, 0);
        assert_eq!(result.record.call_usages.len(), 0);
    }
}
