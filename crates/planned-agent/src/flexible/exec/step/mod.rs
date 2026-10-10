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
use planned_agent_core::mcp::types::{parse_content_blocks, ContentBlock};
use planned_agent_tool_manager::{ToolOutcome, ToolRegistry};
use tokio::sync::watch;

use super::event::{PlanRunEvent, PlanRunSink};
use super::executor::ExecutorConfig;
use super::prompt;
use super::report::{CallUsage, StepRunRecord, StepStatus, ToolCallRecord};
use super::spill;
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
    /// 本次执行的产出目录名（工具大输出落盘用；由执行器生成，见 `exec::spill`）
    pub run_dir: &'a str,
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

/// 拼「工具结果的文本部分 + 图片落盘说明」。
///
/// 文案在这里（而不是 `image.rs`）：它是**回灌提示**的一部分，属于编排；`image.rs`
/// 只负责吐出「每张图存到了哪」。
///
/// 两条措辞纪律：
/// - **不点名工具**：写死 `builtin_recognize_image` 会在该工具没注册 / 将来改名时变成
///   幻觉源；模型从工具表自己就能对上（description 里写明了「读取一张本地图片」）。
/// - **不预设用途、也不指导怎么读**：只声明「这是已落盘的文件」（`file@`），不写「用哪个工具
///   打开它」——「识别验证码」只是图片用途之一（还可能是看布局、提取表格）。**通用**的
///   「按文件类型选相配工具」指引在 `STEP_SYSTEM_PROMPT` 的 `file@` 条，不在此重复；
///   本步具体用哪个工具由意图与工具表决定。
fn join_text_and_image_notes(text: &str, notes: &[String]) -> String {
    // 真实调用点受 `match` 守卫保护（含图片 ⇒ `notes` 非空），这一支只为让本函数
    // 作为「拼接器」自身完备、可脱离执行链路单测。
    if notes.is_empty() {
        return text.to_string();
    }
    let mut rendered = text.to_string();
    if !rendered.is_empty() {
        rendered.push_str("\n\n");
    }
    rendered.push_str(&format!(
        "（工具返回了 {} 张图片，已保存为本地文件：\n{}\n）",
        notes.len(),
        notes.join("\n")
    ));
    rendered
}

/// 工具输出进 messages 的那一份：超过阈值就落盘，改用「文件 + 预览」引用。
///
/// 落盘失败**不**判该步失败（与跨步产出的严格语义刻意不同，见
/// `docs/planned-agent/flexible-step-tool-output-spill.md` §2.7）：工具已经执行完了，
/// 落盘失败只是这条输出多占上下文 —— 全文照旧内联，信息一点没丢。
async fn tool_output_for_llm(
    output: &str,
    input: &StepInput<'_>,
    cfg: &ExecutorConfig,
    round: usize,
    nth: usize,
) -> String {
    // 文件名不含 LLM 给的任何字符串（只含步骤/轮次/序号），没有目录穿越风险。
    // **必须带步骤序号**：`run_dir` 是 run 级目录，而 `round`/`nth` 在每步内从 0 重新计数，
    // 只用后者会让不同步骤的工具输出互相覆盖。
    let file_name = format!("tool-s{}-r{round}-{nth}.txt", input.index);
    match spill::spill_text(
        &cfg.cache_dir,
        input.run_dir,
        &file_name,
        cfg.spill_threshold_chars,
        output,
    )
    .await
    {
        Ok(Some(spilled)) => spill::render_spill_reference(
            output,
            &spilled,
            cfg.spill_preview_chars,
            spill::SpillKind::ToolOutput,
        ),
        Ok(None) => output.to_string(),
        Err(error) => {
            tracing::warn!(
                step = input.index,
                round,
                nth,
                file = %file_name,
                %error,
                "工具输出落盘失败，回退为内联全文"
            );
            output.to_string()
        }
    }
}

/// 调用工具；若「补全后的参数」失败且错误像路径问题，改用模型给的**原参数**重试一次。
///
/// 为什么需要：`call_arguments` 无条件把裸文件名补成产出目录 —— 对**读类**参数
/// （上传文件、加载脚本）这可能补错（文件本在用户地盘 / 工作目录），补错就报「找不到
/// 文件」。这里兜住：**只重试一次**，且**只认路径类错误**（否则工具因别的原因失败也会白
/// 重试）。回退也失败时返回**第一次**的结果 —— 那是「补全后」的事实，信息量更大。
///
/// 返回的 `bool` = 结果是否来自**补全后的参数**（回灌文案据此如实措辞）。
async fn call_tool_with_path_fallback(
    registry: &Arc<ToolRegistry>,
    tool_name: &str,
    rewritten: serde_json::Value,
    original: serde_json::Value,
    rewrites: &[call_arguments::PathRewrite],
) -> (anyhow::Result<ToolOutcome>, bool) {
    let first = registry.call_tool(tool_name, rewritten).await;

    // 没有发生改写 → 结果只可能来自这一次调用。
    if rewrites.is_empty() {
        return (first, false);
    }

    let path_failure = match &first {
        Ok(outcome) if !outcome.result.is_error => false,
        Ok(outcome) => call_arguments::looks_like_path_error(&tool_content(&outcome.result.content)),
        Err(err) => call_arguments::looks_like_path_error(&err.to_string()),
    };
    // 成功、或失败与路径无关 → 就是补全参数的结果。
    if !path_failure {
        return (first, true);
    }

    tracing::info!(
        target: "tool_path_normalization",
        tool = %tool_name,
        rewrites = rewrites.len(),
        "补全后的参数疑似不适用，改用模型给的原参数重试一次"
    );

    match registry.call_tool(tool_name, original).await {
        // 回退成功 → 结果来自原参数。
        Ok(outcome) if !outcome.result.is_error => (Ok(outcome), false),
        // 回退也失败 → 保留补全那次的结果（它更接近「我们做了什么」）。
        _ => (first, true),
    }
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
    // 空回答**重发**的累计次数：**不占** `rounds`（见 `ExecutorConfig::llm_empty_retries`）。
    let mut llm_retries = 0usize;
    let mut output: Option<String> = None;
    let mut error: Option<String> = None;

    'step: loop {
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

        // 本轮最多重发 `cfg.llm_empty_retries` 次（**不占** `rounds`）。每次重发都复用
        // 同一条 `request`：`messages` 一字未动，工具已经执行、结果已回灌 ——
        // 所以重发**不会重复执行工具**，只是让模型把这一轮的话重新说出来。
        let mut retries_this_round = 0usize;
        let (message, tool_calls) = loop {
            // 超时、超时重试、取消即时 都在 `request_llm` 里（见其文档）。
            let response = match request_llm(ai, &request, cfg, cancel, input.index, rounds).await {
                Ok(response) => response,
                Err(reason) => {
                    error = Some(reason);
                    break 'step;
                }
            };

            // token 采集：每次**请求**记一条（缺失记 0）。同一轮的首次请求 `retry = 0`，
            // 空回答重发各记一条 `retry = 1..n` ⇒ `call_usages.len() == rounds + llm_retries`。
            let (prompt_tokens, completion_tokens) = match response.usage {
                Some(usage) => (usage.prompt_tokens, usage.completion_tokens),
                None => (0, 0),
            };
            call_usages.push(CallUsage {
                round: rounds,
                retry: retries_this_round,
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
                break 'step;
            };
            // 诊断用：`finish_reason` 是区分「provider 空响应」与「被 max_tokens 截断」的
            // 唯一线索，必须在 `choice.message` 被 move 之前取出。
            let finish_reason = choice.finish_reason;
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
                    // 空回答是「**无效响应**」而不是「终态失败」：provider 偶发空响应、
                    // 推理预算耗尽被截断都会撞上（实测：一次 2.4s 的空响应让整次执行作废）。
                    // 先按**同一条请求**重发 —— 不重复执行工具、不占轮数；仍空才判失败。
                    if retries_this_round < cfg.llm_empty_retries {
                        retries_this_round += 1;
                        llm_retries += 1;
                        tracing::warn!(
                            step = input.index,
                            round = rounds,
                            attempt = retries_this_round,
                            max_retries = cfg.llm_empty_retries,
                            finish_reason = ?finish_reason,
                            completion_tokens,
                            "模型空回答（无正文 / 无思考 / 无工具调用），重发该轮请求"
                        );
                        continue;
                    }
                    tracing::warn!(
                        step = input.index,
                        round = rounds,
                        retries = retries_this_round,
                        finish_reason = ?finish_reason,
                        completion_tokens,
                        "模型既无正文也无思考内容（空回答），该步失败"
                    );
                    // 重发次数为 0（未开重发或已关闭）时保持**原文案**，改造前后观感一致。
                    error = Some(if retries_this_round == 0 {
                        "模型未产出内容（空回答）".to_string()
                    } else {
                        format!("模型未产出内容（空回答，已重发 {retries_this_round} 次）")
                    });
                    break 'step;
                }
                tracing::info!(
                    step = input.index,
                    rounds,
                    output_chars = answer.chars().count(),
                    "步骤产出完成（无工具调用）"
                );
                output = Some(answer);
                break 'step;
            }

            // 本轮拿到了工具调用 → 结束重发内层，交给外层执行工具
            break (message, tool_calls);
        };

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
        for (nth, call) in tool_calls.into_iter().enumerate() {
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
            // 裸文件名 → 本次执行的产出目录（与工具输出落盘、图片落盘同一个目录）。
            // 只动「传给工具的那一份参数」，不改模型自己的消息记录。
            let original_arguments = arguments.clone();
            let (arguments, path_rewrites) = call_arguments::resolve(
                registry.get_tool(&tool_name).as_ref(),
                arguments,
                &cfg.cache_dir.join(input.run_dir),
            );
            let (call_result, used_rewritten) = call_tool_with_path_fallback(
                registry,
                &tool_name,
                arguments,
                original_arguments,
                &path_rewrites,
            )
            .await;
            let (tool_output, is_error) = match call_result {
                Ok(outcome) => {
                    // 工具结果进 messages 的文本。**类型区分在这里**（不下放给 image 模块）：
                    // 含图片块 → 落盘，base64 就地丢弃，进上下文 / 日志的只有文本 + 绝对路径；
                    // 其余（纯文本 / 自定义 JSON）→ 原渲染路径，语义逐字不变。
                    // 必须在下面的 `output = %content` 与 `messages.push` 之前完成。
                    let blocks = parse_content_blocks(&outcome.result.content);
                    let content = match blocks.as_deref() {
                        // 含图片：图片落盘成文件，进上下文只有文本 + 绝对路径。
                        // 文本部分**必须**从块里取 —— 不能走下面的 `tool_content`，
                        // 它会把含 base64 的数组序列化成 JSON。
                        Some(blocks)
                            if blocks
                                .iter()
                                .any(|block| matches!(block, ContentBlock::Image { .. })) =>
                        {
                            let notes = image::spill_images(
                                blocks,
                                &cfg.cache_dir,
                                input.run_dir,
                                &format!("s{}-r{rounds}-c{nth}", input.index),
                            )
                            .await;
                            let text = blocks
                                .iter()
                                .filter_map(ContentBlock::text)
                                .collect::<Vec<_>>()
                                .join("\n");
                            join_text_and_image_notes(&text, &notes)
                        }
                        _ => tool_content(&outcome.result.content),
                    };
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
            // 大输出落盘：**只换「进 messages 的那一份」**，日志 / 事件 / 报告仍用原文。
            let mut content_for_llm =
                tool_output_for_llm(&tool_output, &input, cfg, rounds, nth).await;
            // 参数被补全过 → 告诉模型实际用了哪条路径（否则改写对模型是「静默」的）。
            // 只进 messages，不进日志 / 事件 / 报告 —— 它是宿主补充，不是工具的真实输出。
            if !path_rewrites.is_empty() {
                content_for_llm
                    .push_str(&call_arguments::render_rewrites(&path_rewrites, used_rewritten));
            }
            messages.push(tool_message(&call.id, &content_for_llm));
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
            llm_retries,
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

mod call_arguments;
mod image;
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
