//! 一次 [`ChatService::send`] 引发的完整多轮对话。
//!
//! 职责拆分：
//! - `stream.rs`：流式 chunk 处理、错误处理、事件发射
//! - `handlers.rs`：UI 工具、后端工具执行
//! - `close.rs`：中断后闭合 tool_calls、补齐孤立消息

pub mod close;
mod handlers;
mod stream;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use futures::StreamExt;
use planned_agent_core::ai::types::{
    ChatCompletionRequest, FunctionCall, Message, MessageContent, MessageRole, ToolCall, ToolType,
};
use planned_agent_core::events::{ChatEvent as CoreChatEvent, UIQuestion};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::bridge::ToolExecutionBridge;
use super::prompt::inject_system_prompt;
use crate::chat::service::ChatEvent;
use crate::chat::state::{Command, State, ToolCallAccumulator};
use crate::chat::tools::{build_tool_definitions, UI_TOOL_NAMES};

use super::confirm::await_confirm;
use close::{close_max_rounds_tool_calls, close_orphaned_user, close_unclosed_tool_calls};
use handlers::{
    execute_backend_tool_call, handle_ui_tool_call, BackendToolResult, UIActionOutcome,
};
use stream::{
    emit_tool_call_completes, log_history_summary, process_stream_chunk, process_stream_error,
};

/// UI 交互策略。
pub(super) enum UIActionStrategy {
    BlockAndConfirm, // ← 主 agent（run_id=None）走这条路：driver 内原地阻塞等 confirm
    EmitAndSuspend,  // ← 子 agent（run_id=Some）走这条路：emit 后挂起交给父 UI
}

/// 对话结果。
pub(super) enum ConversationOutcome {
    Completed,
    Suspended { run_id: String },
}

/// 达到 `max_tool_rounds` 询问用户后，本次触顶的处理去向。
#[derive(Debug, Clone)]
enum PromptChoice {
    /// 用户选择继续：放行本批尚未执行的 tool_calls（多续一轮）。
    Continue,
    /// 用户选择结束 / 无法询问：闭合本批并终止循环。
    Stop,
    /// 子 agent：本批真实工具已 close，转入一次 request_user_action 询问回合并挂起，
    /// 由父 UI 呈现卡片、用户作答后经 resume 恢复。
    Suspend { run_id: String },
}

/// 构造「是否继续执行」询问卡的问题文本与问题项。
fn continue_question(max_rounds: usize) -> (String, Vec<UIQuestion>) {
    let questions = vec![UIQuestion {
        header: "继续？".to_string(),
        question: format!(
            "对话已连续调用工具 {max_rounds} 轮，达到轮次上限。任务可能未完成，是否继续执行？"
        ),
        options: vec![
            planned_agent_core::events::UIOption {
                label: "继续执行".to_string(),
                description: None,
                value: Some("continue".to_string()),
            },
            planned_agent_core::events::UIOption {
                label: "结束".to_string(),
                description: Some("停止当前任务".to_string()),
                value: Some("stop".to_string()),
            },
        ],
        multi: false,
        allow_input: false,
    }];
    (
        format!("已达最大轮次上限（{max_rounds} 轮）。任务可能未完成，是否继续执行？"),
        questions,
    )
}

/// 达到 `max_tool_rounds` 时的处理。
///
/// 复用 request_user_action 卡片协议向用户问「是否继续执行」：
/// - 主 agent（`BlockAndConfirm`）：driver 内原地阻塞等用户；选「继续」→ 放行本批尚未
///   执行的 tool_calls（不重置 round，执行后 `round += 1`，下轮若仍达上限会再次询问 =
///   每次续一轮、可反复弹）；选「结束」→ 闭合本批并终止。
/// - 子 agent（`EmitAndSuspend`）：driver 内不能原地阻塞，必须挂起交给父 UI。先把本批
///   真实工具 close（未执行过，无重复），再合成一条 request_user_action 询问回合写入
///   history 并挂起——resume 以 history 里这条「未闭合的 request_user_action」为锚点；
///   用户作答后经 resume 触发新一次 run_conversation（round 从 1 重算，即"继续"再给一档
///   新预算跑到底，到上限再弹）。
async fn prompt_max_rounds<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    queue: &mut VecDeque<Command>,
    ui_strategy: &UIActionStrategy,
    max_rounds: usize,
) -> Result<PromptChoice> {
    // ── request_user_action 询问（两种策略共用同一卡语义） ──
    const UI_ID: &str = "max_rounds_continue";
    let (ask_message, ask_questions) = continue_question(max_rounds);

    if matches!(ui_strategy, UIActionStrategy::EmitAndSuspend) {
        // 子 agent：先 close 本批真实工具（未执行，无重复），再合成 request_user_action 回合。
        close_max_rounds_tool_calls(state);
        let call = ToolCall {
            id: UI_ID.to_string(),
            r#type: ToolType::Function,
            function: FunctionCall {
                name: "request_user_action".to_string(),
                arguments: serde_json::json!({
                    "message": ask_message,
                    "questions": ask_questions,
                })
                .to_string(),
            },
        };
        state.history.push_assistant(Message {
            role: MessageRole::Assistant,
            content: None,
            tool_calls: Some(vec![call.clone()]),
            ..Default::default()
        });
        return match handle_ui_tool_call(state, &call, ui_strategy, rx, queue, "").await? {
            UIActionOutcome::Suspended { run_id } => {
                info!("[round] 子 agent 触顶：挂起等用户选择是否继续");
                Ok(PromptChoice::Suspend { run_id })
            }
            // EmitAndSuspend 理论上只返回 Suspended；其余兜底按结束处理。
            _ => Ok(PromptChoice::Stop),
        };
    }

    // ── 主 agent：合成 request_user_action 的 ToolCallStart + UIActionRequest，原地 await ──
    // 该回合不写入 history（用户选择只决定控制流，不回喂给 LLM）。
    state
        .subscribers
        .emit(ChatEvent::Chat(CoreChatEvent::ToolCallStart {
            id: UI_ID.to_string(),
            name: "request_user_action".to_string(),
            source: None,
        }));
    let session_id = state.config.lock().unwrap().run_id.clone();
    state
        .subscribers
        .emit(ChatEvent::Chat(CoreChatEvent::UIActionRequest {
            message: ask_message,
            questions: ask_questions,
            session_id,
        }));

    match await_confirm(state, rx, queue, UI_ID).await? {
        Some((choice, _action_id)) if choice.contains("continue") => {
            info!("[round] 触顶后用户选择继续执行（再续一轮）");
            Ok(PromptChoice::Continue)
        }
        Some(_) => {
            info!("[round] 触顶后用户选择结束");
            Ok(PromptChoice::Stop)
        }
        None => {
            info!("[round] 触顶询问被取消（cancelled）");
            Ok(PromptChoice::Stop)
        }
    }
}

/// 运行一次完整的多轮对话循环。
pub(super) async fn run_conversation<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    queue: &mut VecDeque<Command>,
    ui_strategy: UIActionStrategy,
    bridge: &dyn ToolExecutionBridge,
) -> Result<ConversationOutcome> {
    inject_system_prompt(state).await?;

    let mut round = 1usize;
    let mut stream_error_text: Option<String> = None;

    loop {
        info!("[round] === 第 {} 轮开始 ===", round);
        if state.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            info!("[round] 已取消，break");
            break;
        }

        state
            .subscribers
            .emit(ChatEvent::Chat(CoreChatEvent::RoundStart { round }));

        let tools = build_tool_definitions(state);

        let (temperature, max_tokens) = {
            let cfg = state.config.lock().unwrap();
            (cfg.temperature, cfg.max_tokens)
        };
        let messages = state.history.snapshot();
        info!("[round] 请求历史: {} 条消息", messages.len());
        for (i, m) in messages.iter().enumerate() {
            info!(
                "[round]   [{}] role={:?} content={} tool_calls={}",
                i,
                m.role,
                m.content
                    .as_ref()
                    .map(|c| format!("{:?}", c).chars().take(50).collect::<String>())
                    .unwrap_or("None".into()),
                m.tool_calls.as_ref().map(|t| t.len()).unwrap_or(0)
            );
        }
        let req = ChatCompletionRequest {
            model: state.ai_client.model_name().to_string(),
            messages,
            tools: Some(tools),
            temperature,
            max_tokens,
            stream: true,
            extra: Default::default(),
        };

        let response_stream = state
            .ai_client
            .chat_completion_stream(req)
            .await
            .map_err(|e| anyhow!("chat_completion_stream 失败: {:?}", e))?;
        let mut inner = response_stream.stream;
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut has_content = false;
        let mut has_reasoning = false;
        let mut accumulators: BTreeMap<u32, ToolCallAccumulator> = BTreeMap::new();

        let mut consecutive_stream_errors = 0u32;
        let mut last_stream_error = String::new();
        const MAX_STREAM_ERRORS: u32 = 5;
        while let Some(chunk_result) = inner.next().await {
            if state.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            match chunk_result {
                Ok(c) => {
                    consecutive_stream_errors = 0;
                    process_stream_chunk(
                        &state.subscribers,
                        &state.tool_registry,
                        c,
                        &mut text,
                        &mut reasoning,
                        &mut has_content,
                        &mut has_reasoning,
                        &mut accumulators,
                    );
                }
                Err(e) => {
                    if process_stream_error(
                        &mut consecutive_stream_errors,
                        &mut last_stream_error,
                        MAX_STREAM_ERRORS,
                        e,
                    ) {
                        break;
                    }
                }
            }
        }

        let tool_calls_vec: Vec<ToolCall> = accumulators
            .values()
            .filter(|acc| !acc.id.is_empty())
            .map(|acc| ToolCall {
                id: acc.id.clone(),
                r#type: ToolType::Function,
                function: FunctionCall {
                    name: acc.name.clone(),
                    arguments: acc.arguments.clone(),
                },
            })
            .collect();

        if consecutive_stream_errors > 0 && !has_content && tool_calls_vec.is_empty() {
            state.subscribers.emit(ChatEvent::Error(last_stream_error.clone()));
            stream_error_text = Some(format!("Error: {}", last_stream_error));
        }

        emit_tool_call_completes(&state.subscribers, &accumulators);

        let assistant_msg = Message {
            role: MessageRole::Assistant,
            content: if has_content {
                Some(MessageContent::Text { text })
            } else {
                None
            },
            tool_calls: if tool_calls_vec.is_empty() {
                None
            } else {
                Some(tool_calls_vec.clone())
            },
            tool_call_id: None,
            name: None,
            reasoning_content: if has_reasoning { Some(reasoning) } else { None },
            ..Default::default()
        };
        // 跳过空消息（无 content 且无 tool_calls）
        if !has_content && tool_calls_vec.is_empty() {
            info!("[round] LLM 返回空消息，跳过写入");
            break;
        }

        // 统一写入，保留 tool_calls（包括 request_user_action）
        state.history.push_assistant(assistant_msg.clone());
        state
            .subscribers
            .emit(ChatEvent::Chat(CoreChatEvent::RoundEnd {
                message: assistant_msg,
            }));

        log_history_summary(&state.history, round);

        if tool_calls_vec.is_empty() {
            info!("[round] 无 tool_calls，break（本轮 LLM 未调用工具）");
            break;
        }

        let max_rounds = state.config.lock().unwrap().max_tool_rounds;
        if round >= max_rounds {
            match prompt_max_rounds(state, rx, queue, &ui_strategy, max_rounds).await? {
                PromptChoice::Continue => {
                    // 用户选择继续：放行本批尚未执行的 tool_calls（多续一轮）。
                    // 本批从未执行过，直接落入下方 partition/执行即可，不会重复执行。
                    info!("[round] 继续执行本批 {} 个工具调用", tool_calls_vec.len());
                }
                PromptChoice::Stop => {
                    warn!("chat: 达到 max_tool_rounds={}, 用户选择结束，终止循环", max_rounds);
                    close_max_rounds_tool_calls(state);
                    break;
                }
                PromptChoice::Suspend { run_id } => {
                    // 子 agent 触顶：本批已在 prompt_max_rounds 内 close，且已合成
                    // request_user_action 询问回合并 emit UIActionRequest，挂起交给父 UI。
                    info!("[round] 子 agent 触顶：挂起等用户选择是否继续（run_id={run_id}）");
                    return Ok(ConversationOutcome::Suspended { run_id });
                }
            }
        }

        let (ui_calls, backend_calls): (Vec<_>, Vec<_>) = tool_calls_vec
            .iter()
            .partition(|tc| UI_TOOL_NAMES.contains(&tc.function.name.as_str()));

        info!(
            "[round] 后端工具 {} 个, UI 工具 {} 个",
            backend_calls.len(),
            ui_calls.len()
        );
        for call in &backend_calls {
            if state.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            match execute_backend_tool_call(state, call, bridge).await? {
                BackendToolResult::Done => {}
                BackendToolResult::Cancelled => {
                    close_unclosed_tool_calls(state);
                    return Ok(ConversationOutcome::Completed);
                }
            }
        }
        info!("[round] 所有后端工具执行完毕，round += 1，继续循环");

        for call in &ui_calls {
            if state.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            match handle_ui_tool_call(state, call, &ui_strategy, rx, queue, &last_stream_error)
                .await?
            {
                UIActionOutcome::Continue => {}
                UIActionOutcome::Invalid { reason } => {
                    // UI 工具无效（如空 questions）：handle 内已闭合该 tool_call 并 emit Error，
                    // 这里直接结束本轮，不再挂起等用户——否则会话永久卡死。
                    info!("[round] UI 工具无效，结束本轮: {}", reason);
                    close_unclosed_tool_calls(state);
                    return Ok(ConversationOutcome::Completed);
                }
                UIActionOutcome::UserCancelled => {
                    close_unclosed_tool_calls(state);
                    return Ok(ConversationOutcome::Completed);
                }
                UIActionOutcome::Suspended { run_id } => {
                    close_unclosed_tool_calls(state);
                    return Ok(ConversationOutcome::Suspended { run_id });
                }
            }
        }

        round += 1;
    }

    // ── 中断后闭合：确保最后一条 assistant 消息的所有 tool_calls 都有对应的 tool 消息 ──
    close_unclosed_tool_calls(state);
    // ── 中断后补齐：若最后一条不是 Assistant，补一条占位消息 ──
    let close_text = stream_error_text.as_deref().unwrap_or("Output interrupt");
    close_orphaned_user(state, close_text);

    Ok(ConversationOutcome::Completed)
}
