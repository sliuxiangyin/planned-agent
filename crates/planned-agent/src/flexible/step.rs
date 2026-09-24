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

/// 单步**完整输出**进执行记录的上限（字符）。
///
/// 摘要（`SUMMARY_MAX_CHARS`）用于步骤间传播；这一份是给「结果展示 + 输出整理步」用的，
/// 所以宽松得多 —— 但仍必须封顶：事件与快照是全量推送，不封顶会把推送量撑爆。
pub(crate) const OUTPUT_MAX_CHARS: usize = 8_000;

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

/// 执行一个步骤（模板步骤：单步 system prompt）。
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
    run_step_with_prompt(prompt::STEP_SYSTEM_PROMPT, input, ai, registry, cfg, sink, cancel).await
}

/// 执行「输出整理步」：同一条执行链路，只换 system prompt。
///
/// 与模板步骤的差别只有提示词（[`prompt::OUTPUT_RESOLVE_SYSTEM_PROMPT`]）—— 调用方传
/// 空工具表即可（整理只做分析，不需要外部数据）。轮数上限、token 计量、轨迹、失败语义
/// 全部复用同一套，不维护第二份。
pub(crate) async fn run_output_resolve(
    input: StepInput<'_>,
    ai: &Arc<dyn AiClient>,
    registry: &Arc<ToolRegistry>,
    cfg: &ExecutorConfig,
    sink: &dyn PlanRunSink,
    cancel: Option<&watch::Receiver<bool>>,
) -> StepRunResult {
    run_step_with_prompt(
        prompt::OUTPUT_RESOLVE_SYSTEM_PROMPT,
        input,
        ai,
        registry,
        cfg,
        sink,
        cancel,
    )
    .await
}

/// 单步执行的内核：system prompt 由调用方决定。
async fn run_step_with_prompt(
    system_prompt: &str,
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
        text_message(MessageRole::System, system_prompt),
        text_message(MessageRole::User, &task),
    ];

    let mut call_usages: Vec<CallUsage> = Vec::new();
    let mut tool_call_count = 0usize;
    let mut rounds = 0usize;
    let mut output: Option<String> = None;
    let mut error: Option<String> = None;

    loop {
        if is_cancelled(cancel) {
            tracing::info!(step = input.index, round = rounds, "收到取消信号，中止该步");
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
                // 失败原因必须同时进日志：报告里的原因只有 UI 看得到，事后查问题只能靠日志。
                tracing::error!(
                    step = input.index,
                    round = rounds,
                    error = %err,
                    "LLM 调用失败，该步失败"
                );
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
            tracing::error!(
                step = input.index,
                round = rounds,
                "LLM 响应不含 choices，该步失败"
            );
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
            tracing::info!(
                step = input.index,
                rounds,
                output_chars = answer.chars().count(),
                "步骤产出完成（无工具调用）"
            );
            output = Some(answer);
            break;
        }

        // 已达轮数上限：不再执行工具，直接判失败（避免无界循环）
        if rounds >= cfg.max_rounds_per_step {
            tracing::warn!(
                step = input.index,
                rounds,
                max_rounds = cfg.max_rounds_per_step,
                tool_calls = tool_call_count,
                "已达每步轮数上限且末轮仍要求调用工具，该步失败"
            );
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
                tracing::info!(step = input.index, round = rounds, "工具循环中收到取消信号");
                error = Some("用户取消".to_string());
                break;
            }

            let arguments = parse_arguments(&call.function.arguments);
            let tool_name = call.function.name.clone();
            // 入参是排查工具失败的**唯一现场证据**：错误信息只说「找不到指定的路径」/「program not
            // found」，而「它到底传了哪条路径 / 传的命令长什么样」只在入参里。渲染一次，三处复用。
            let args_desc = describe_arguments(&arguments);
            // `$` 行只给「关键入参」：短、像命令行（与日志用的全量 JSON 分开）。
            let args_line = describe_tool_args(&arguments);
            tracing::debug!(
                step = input.index,
                round = rounds,
                tool = %tool_name,
                args = %args_desc,
                "工具调用入参"
            );
            let (tool_output, is_error) = match registry.call_tool(&tool_name, arguments).await {
                Ok(outcome) => {
                    let content = tool_content(&outcome.result.content);
                    if outcome.result.is_error {
                        // 工具报错不一定让该步失败（结果会回灌给 LLM 继续决策），但必须留痕
                        tracing::warn!(
                            step = input.index,
                            round = rounds,
                            tool = %tool_name,
                            args = %args_desc,
                            output = %content,
                            "工具返回了错误结果，已回灌给 LLM"
                        );
                    }
                    (content, outcome.result.is_error)
                }
                Err(err) => {
                    tracing::warn!(
                        step = input.index,
                        round = rounds,
                        tool = %tool_name,
                        args = %args_desc,
                        error = %err,
                        "工具执行失败，错误信息已回灌给 LLM"
                    );
                    (format!("工具执行失败：{err}"), true)
                }
            };

            tool_call_count += 1;
            tracing::debug!(
                step = input.index,
                round = rounds,
                tool = %tool_name,
                ok = !is_error,
                "工具调用完成"
            );
            sink.emit(PlanRunEvent::StepToolCall {
                index: input.index,
                tool: tool_name,
                args: args_line,
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
    // 完整输出单独封顶：`StepRunResult.output` 保持**不截断**（下游 `prior` 依赖它，
    // 现有语义不动），执行记录里只存封顶后的一份，并如实带上「被截断」标记。
    let output_truncated = output
        .as_deref()
        .map(|text| text.chars().count() > OUTPUT_MAX_CHARS)
        .unwrap_or(false);
    let record_output = output
        .as_deref()
        .map(|text| truncate_chars(text, OUTPUT_MAX_CHARS));

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
            output: record_output,
            output_truncated,
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

/// 把工具入参渲染成一行日志文本（超长截断）。
///
/// 上限故意给得比输出摘要宽：入参是排查失败的现场证据，截太狠会丢掉关键那一截；
/// 但 `write_file` 之类的入参会带上整篇文件正文，完全不截断又会淹掉日志。
fn describe_arguments(arguments: &Value) -> String {
    const ARG_LOG_MAX_CHARS: usize = 800;
    let rendered = arguments.to_string();
    let total = rendered.chars().count();
    if total <= ARG_LOG_MAX_CHARS {
        return rendered;
    }
    let head: String = rendered.chars().take(ARG_LOG_MAX_CHARS).collect();
    format!("{head}…（已截断，共 {total} 字符）")
}

/// 把工具入参渲染成 `$` 行上的「关键参数」。
///
/// 与 [`describe_arguments`]（日志用、全量 JSON）不同，这里追求**像一条命令行**：
/// `{"command":"ls","args":["C:/x"]}` → `ls C:/x`，`{"path":"C:/x"}` → `C:/x`。
/// 太长会毁掉终端的可读性，故单行截断。
fn describe_tool_args(arguments: &Value) -> String {
    const ARGS_LINE_MAX_CHARS: usize = 120;

    let rendered = match arguments {
        Value::Object(map) => {
            // ① 「程序 + 参数」是执行类入参的常见形态，直接拼成命令行。
            if let Some(Value::String(command)) = map.get("command") {
                let args = match map.get("args") {
                    Some(Value::Array(items)) => {
                        items.iter().map(tool_content).collect::<Vec<_>>().join(" ")
                    }
                    Some(other) => tool_content(other),
                    None => String::new(),
                };
                format!("{command} {args}").trim_end().to_string()
            } else if map.len() == 1 {
                // ② 单字段（`{"path": ...}` / `{"query": ...}`）直接给值，读起来最像参数。
                map.values().next().map(tool_content).unwrap_or_default()
            } else {
                // ③ 其余退回紧凑 JSON：字段名本身有信息量，不该丢。
                arguments.to_string()
            }
        }
        other => tool_content(other),
    };

    if rendered.chars().count() <= ARGS_LINE_MAX_CHARS {
        return rendered;
    }
    let head: String = rendered.chars().take(ARGS_LINE_MAX_CHARS).collect();
    format!("{head}…")
}

/// 工具输出转文本：字符串取原文，其余取 JSON 字面量。
fn tool_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// 生成输出摘要（超长截断）。
/// 按**字符**（而非字节）截断：UTF-8 安全，不 panic。
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

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

    /// 一段可断言的日志缓冲区（配合 `tracing` 的线程局部订阅者使用）。
    #[derive(Clone, Default)]
    struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

    impl CapturedLog {
        /// 在当前线程装上订阅者；返回的 guard 析构即卸载。
        fn install(&self) -> tracing::subscriber::DefaultGuard {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(self.clone())
                .with_max_level(tracing::Level::DEBUG)
                .with_ansi(false)
                .without_time()
                .finish();
            tracing::subscriber::set_default(subscriber)
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("日志缓冲区锁被毒化")).to_string()
        }
    }

    impl std::io::Write for CapturedLog {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("日志缓冲区锁被毒化")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
        type Writer = CapturedLog;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// 工具执行失败时，日志必须带上 **LLM 实际传的入参**。
    ///
    /// 这是失败现场唯一的证据：错误信息只会说「找不到指定的路径」/「program not found」，
    /// 而「它到底传了哪条路径、哪条命令」只在入参里 —— 少了它就只能靠错误码反推。
    #[tokio::test]
    async fn failed_tool_call_logs_its_arguments() {
        let captured = CapturedLog::default();
        let _guard = captured.install();

        // 故意调一个没注册的工具：`call_tool` 走 Err 分支（与 builtin_execute_command
        // 在 Windows 上拿不到可执行文件时是同一条路径）。
        let ai = FakeAiClient::new(vec![tool_response(
            "call-1",
            "builtin_execute_command",
            json!({"command": "echo 1 >> text.txt", "args": []}),
            10,
            5,
        )]);
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

        assert!(result.record.tool_calls > 0, "应当发生过工具调用");
        let log = captured.text();
        // 精确到「失败那一行」：缓冲区里另有一条 debug 级「工具调用入参」，
        // 若只断言整段日志含入参，就分不清失败行自己到底带没带。
        let failed_line = log
            .lines()
            .find(|line| line.contains("工具执行失败"))
            .unwrap_or_else(|| panic!("没有记下「工具执行失败」这一行：{log}"));
        assert!(
            failed_line.contains("echo 1 >> text.txt"),
            "失败行必须带上 LLM 实际传的入参，否则无从分析：{failed_line}"
        );
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

        // 事件流：StepToolCall(ok=true) 出现，且带着 `$` 行要显示的关键入参
        let events = sink.events();
        assert!(events.iter().any(|event| matches!(
            event,
            PlanRunEvent::StepToolCall { tool, ok: true, .. } if tool == "read"
        )));
        assert!(
            events.iter().any(|event| matches!(
                event,
                PlanRunEvent::StepToolCall { args, .. } if args == "a.txt"
            )),
            "事件要带上渲染好的关键入参（单个 path 字段直接给值）：{events:?}"
        );
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

    /// `$` 行的渲染规则：要像一条命令行，且不能把终端撑爆。
    #[test]
    fn tool_args_render_like_a_command_line() {
        // 「程序 + 参数」拼成命令行
        assert_eq!(
            describe_tool_args(&json!({"command": "ls", "args": ["C:/x"]})),
            "ls C:/x"
        );
        // 单字段直接给值（不带 JSON 括号）—— `{"path": ...}` 是最常见的形态
        assert_eq!(
            describe_tool_args(&json!({"path": "C:/Users/x/a.txt"})),
            "C:/Users/x/a.txt"
        );
        assert_eq!(describe_tool_args(&json!({"command": "dir"})), "dir");
        // 字段多且不认识 → 退回紧凑 JSON（字段名本身有信息量，不该丢）
        assert_eq!(
            describe_tool_args(&json!({"a": 1, "b": 2})),
            "{\"a\":1,\"b\":2}"
        );
        // 超长截断
        let rendered = describe_tool_args(&json!({ "path": "x".repeat(400) }));
        assert!(
            rendered.chars().count() <= 121,
            "超长入参应截断，实际 {} 字符",
            rendered.chars().count()
        );
    }
}
