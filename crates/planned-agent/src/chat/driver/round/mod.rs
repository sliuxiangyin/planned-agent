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
use planned_agent_core::events::ChatEvent as CoreChatEvent;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::bridge::ToolExecutionBridge;
use super::prompt::inject_system_prompt;
use crate::chat::service::ChatEvent;
use crate::chat::state::{Command, State, ToolCallAccumulator};
use crate::chat::tools::{build_tool_definitions, UI_TOOL_NAMES};

use close::{close_max_rounds_tool_calls, close_orphaned_user, close_unclosed_tool_calls};
use handlers::{execute_backend_tool_call, handle_ui_tool_call, BackendToolResult, UIActionOutcome};
use stream::{emit_tool_call_completes, log_history_summary, process_stream_chunk, process_stream_error};

/// UI 交互策略。
pub(super) enum UIActionStrategy {
    BlockAndConfirm,
    EmitAndSuspend,
}

/// 对话结果。
pub(super) enum ConversationOutcome {
    Completed,
    Suspended { run_id: String },
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
            .map_err(|e| anyhow!("chat_completion_stream 失败: {}", e))?;
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
            state.subscribers.emit(ChatEvent::Error(last_stream_error));
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

        if round >= state.config.lock().unwrap().max_tool_rounds {
            warn!(
                "chat: 达到 max_tool_rounds={}, 终止循环",
                state.config.lock().unwrap().max_tool_rounds
            );
            close_max_rounds_tool_calls(state);
            break;
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
            match handle_ui_tool_call(state, call, &ui_strategy, rx, queue).await? {
                UIActionOutcome::Continue => {}
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
    close_orphaned_user(state, "Output interrupt");

    Ok(ConversationOutcome::Completed)
}
