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
use crate::chat::storage::ErrorType;
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

/// 为触顶询问生成唯一的 request_user_action tool_call id（多次触顶也能区分闭合）。
fn next_max_rounds_ui_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!("max_rounds_continue_{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// 达到 `max_tool_rounds` 时的处理（写入历史的真实 request_user_action 回合）。
///
/// 与 request_user_action_demo 里 LLM 主动发起的交互一致：把「是否继续」作为一条完整工具回合
/// 写入 history —— `assistant(tool_calls=[request_user_action])` + 用户选择的 `tool`，因此可持久化、
/// 可回显，前端交互卡也有对应锚点。
/// - 触顶时本批真实工具一律先 close（主/子一致，本批不执行）。
/// - 主 agent（`BlockAndConfirm`）：原地 await；用户作答后 push_tool 闭合该回合。选「继续」→
///   外层授予 1 次"续跑额度"，下一批工具放行执行一轮后再回到上限重新询问（每次续一轮、可反复弹）；
///   选「结束」→ 结束。
/// - 子 agent（`EmitAndSuspend`）：emit 请求后挂起，resume 以 history 里这条未闭合的
///   request_user_action 为锚点闭合，再跑（round 重算，即"继续"给一档新预算）。
async fn prompt_max_rounds<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    queue: &mut VecDeque<Command>,
    ui_strategy: &UIActionStrategy,
    max_rounds: usize,
    round: usize,
) -> Result<PromptChoice> {
    let (ask_message, ask_questions) = continue_question(max_rounds);

    // 触顶：本批真实工具一律先按「达到轮次限制」close（主/子一致，本批不执行）。
    close_max_rounds_tool_calls(state);

    // 合成 request_user_action 回合并写入历史（唯一 id，保证多次触顶的 assistant/tool 能对应）。
    let ui_id = next_max_rounds_ui_id();
    let call = ToolCall {
        id: ui_id.clone(),
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

    if matches!(ui_strategy, UIActionStrategy::EmitAndSuspend) {
        // 子 agent：不能原地阻塞。emit 请求后挂起交给父 UI；resume 时 driver 以这条
        // 未闭合的 request_user_action 为锚点 push_tool 闭合。
        let run_id = state.config.lock().unwrap().run_id.clone().unwrap_or_default();
        state
            .subscribers
            .emit(ChatEvent::Chat(CoreChatEvent::UIActionRequest {
                message: ask_message,
                questions: ask_questions,
                session_id: Some(run_id.clone()),
            }));
        info!("[round] 子 agent 触顶：request_user_action 已写入历史并挂起（id={ui_id}）");
        return Ok(PromptChoice::Suspend { run_id });
    }

    // ── 主 agent：原地 await，用户作答后闭合写入历史 ──
    // 触顶合成的 request_user_action 是后端额外插入的一条「回合」。此时该轮已 RoundEnd
    // （非 streaming），前端只靠事件不会为它建气泡；补发一次 RoundStart，让前端把它当作
    // 一次新回合、建出独立占位气泡——choice 由既有 append_to_last_assistant 落到这条气泡，
    // 顺序位于上一条真实工具回合之后，实时与回显一致（子 agent 分支不走这里，不污染父气泡）。
    state
        .subscribers
        .emit(ChatEvent::Chat(CoreChatEvent::RoundStart { round }));
    state
        .subscribers
        .emit(ChatEvent::Chat(CoreChatEvent::ToolCallStart {
            id: ui_id.clone(),
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

    match await_confirm(state, rx, queue, &ui_id).await? {
        Some((choice, action_id)) => {
            let tool_content = serde_json::json!({ "choice": choice.clone(), "action_id": action_id });
            state.history.push_tool(&ui_id, &tool_content, ErrorType::None);
            if choice.contains("continue") {
                info!("[round] 触顶后用户选择继续执行（id={ui_id}）");
                Ok(PromptChoice::Continue)
            } else {
                info!("[round] 触顶后用户选择结束");
                Ok(PromptChoice::Stop)
            }
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
    // 用户触顶选择「继续」后授予的续跑额度：>0 时下一次触顶放行本批执行一轮，然后清零。
    let mut grace_rounds: usize = 0;

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
            if grace_rounds > 0 {
                // 上轮用户选「继续」给的续跑额度：放行本批真实工具执行一轮。
                grace_rounds -= 1;
                info!("[round] 消耗续跑额度，放行本批 {} 个工具调用", tool_calls_vec.len());
            } else {
                match prompt_max_rounds(state, rx, queue, &ui_strategy, max_rounds, round).await? {
                    PromptChoice::Continue => {
                        // 用户选「继续」：本批已在 prompt_max_rounds 内 close（成 cancelled），
                        // 授予 1 次续跑额度供下一批放行，然后进入下一轮让 LLM 继续发起。
                        grace_rounds = 1;
                        info!("[round] 用户选择继续：本批已 close，授权续跑一轮");
                        round += 1;
                        continue;
                    }
                    PromptChoice::Stop => {
                        // 本批已在 prompt_max_rounds 内 close（成 cancelled）。
                        warn!("chat: 达到 max_tool_rounds={}, 用户选择结束，终止循环", max_rounds);
                        break;
                    }
                    PromptChoice::Suspend { run_id } => {
                        info!("[round] 子 agent 触顶：挂起等用户选择是否继续（run_id={run_id}）");
                        return Ok(ConversationOutcome::Suspended { run_id });
                    }
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
