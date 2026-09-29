//! 单步执行：一次「LLM ⇄ 工具」循环。
//!
//! 每步都是自包含的：全新消息列表、全新循环，与前序步骤只通过结果表交换数据。
//! 这是本模块唯一"自己写的循环" —— 为的是把 token / 耗时留在自己手里
//! （走 `chat` 的 `ChatService` 会在 driver 内部丢掉 `usage`）。

use std::sync::Arc;
use std::time::Instant;

use planned_agent_core::ai::types::{
    ChatCompletionRequest, MessageRole, ToolDefinition,
};
use planned_agent_core::ai::AiClient;
use planned_agent_tool_manager::ToolRegistry;
use serde_json::Value;
use tokio::sync::watch;

use super::event::{PlanRunEvent, PlanRunSink};
use super::executor::ExecutorConfig;
use super::prompt;
use super::report::{CallUsage, StepRunRecord, StepStatus, ToolCallRecord};
use super::super::plan::template::PlanStep;

/// 输出摘要的字符上限。
const SUMMARY_MAX_CHARS: usize = 200;

/// 单步**完整输出**进执行记录的上限（字符）。
///
/// 摘要（`SUMMARY_MAX_CHARS`）用于步骤间传播；这一份是给「结果展示 + 输出整理步」用的，
/// 所以宽松得多 —— 但仍必须封顶：事件与快照是全量推送，不封顶会把推送量撑爆。
pub(crate) const OUTPUT_MAX_CHARS: usize = 8_000;

/// 单步执行的输入。
pub(crate) struct StepInput<'a> {
    /// 步骤定义（**模板原文**：`expected_output` 保持未展开，供记录与 UI 用）
    pub step: &'a PlanStep,
    /// 已展开占位符的 `intent`
    pub intent: &'a str,
    /// 已展开占位符的 `expected_output`
    ///
    /// 与 `step.expected_output`（模板原文）的区别：**这一份才是进 prompt 的**。
    /// 两者为何分开见 `params::render_step_expected_output`。
    pub expected_output: &'a str,
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
    system_prompt: &str,
    input: StepInput<'_>,
    ai: &Arc<dyn AiClient>,
    registry: &Arc<ToolRegistry>,
    cfg: &ExecutorConfig,
    sink: &dyn PlanRunSink,
    cancel: Option<&watch::Receiver<bool>>,
) -> StepRunResult {
    run_step_with_prompt(system_prompt, input, ai, registry, cfg, sink, cancel).await
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
    let task = prompt::build_step_task(input.intent, input.expected_output, input.prior);

    let mut messages = vec![
        text_message(MessageRole::System, system_prompt),
        text_message(MessageRole::User, &task),
    ];

    let mut call_usages: Vec<CallUsage> = Vec::new();
    let mut tool_call_count = 0usize;
    // C1：工具序列 —— 与 `StepToolCall` 事件**同一处**采集（一处采集、两条出口）。
    let mut tool_sequence: Vec<ToolCallRecord> = Vec::new();
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

        // 超时、超时重试、取消即时 都在 `request_llm` 里（见其文档）。
        let response = match request_llm(ai, &request, cfg, cancel, input.index, rounds).await {
            Ok(response) => response,
            Err(reason) => {
                error = Some(reason);
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
            // 空产出不能当成功：`Done` 的判据是「有产出」（见下方 status 计算），
            // 若放一个空串进 store，下游 `prior` 会拿到空数据、甚至成为最终 result。
            if answer.trim().is_empty() {
                tracing::warn!(
                    step = input.index,
                    round = rounds,
                    "模型既无正文也无思考内容（空回答），该步失败"
                );
                error = Some("模型未产出内容（空回答）".to_string());
                break;
            }
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
            let ok = !is_error;
            tracing::debug!(
                step = input.index,
                round = rounds,
                tool = %tool_name,
                ok,
                "工具调用完成"
            );
            // C1：与下面的事件**同处**采集，但**用途不同** —— 事件那条 `$` 行的入参
            // （`args_line`）截到 120 字符是给 UI 看的；报告这条用 `args_desc`（全量 JSON）
            // 是给排查/统计看的。**必须在 `emit` 之前 clone**：`tool_name` / `args_line`
            // 会被 move 进事件。
            tool_sequence.push(ToolCallRecord {
                tool: tool_name.clone(),
                args: args_desc.clone(),
                ok,
            });
            sink.emit(PlanRunEvent::StepToolCall {
                index: input.index,
                tool: tool_name,
                args: args_line,
                ok,
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
            tool_sequence,
            rounds,
            call_usages,
            output_summary,
            output: record_output,
            output_truncated,
            // 落盘由执行器在拿到产出后决定（它才知道 cache_dir 与阈值）
            output_file: None,
            error,
        },
        output,
    }
}

mod llm;
mod render;

// 内部模块的函数经此转出：`mod.rs` 的 `run_step_with_prompt` 要用，测试的
// `use super::*` 也靠它拿到（私有 `use` 传不到子模块）。
pub(crate) use llm::{is_cancelled, request_llm};
pub(crate) use render::{
    content_text, describe_arguments, describe_tool_args, parse_arguments, summarize, text_message,
    tool_content, tool_message, truncate_chars,
};

#[cfg(test)]
mod tests;
