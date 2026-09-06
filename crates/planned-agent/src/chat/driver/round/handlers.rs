//! 工具调用处理：UI 工具、后端工具执行。

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use planned_agent_core::ai::types::ToolCall;
use planned_agent_core::events::ChatEvent as CoreChatEvent;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::super::bridge::ToolExecutionBridge;
use super::super::confirm::await_confirm;
use super::close::close_tool_calls_with_reason;
use super::UIActionStrategy;
use crate::chat::service::ChatEvent;
use crate::chat::state::{Command, State};
use crate::chat::storage::ErrorType;
use crate::chat::tools::parse_ui_questions;

/// UI 工具调用结果。
pub(super) enum UIActionOutcome {
    Continue,
    UserCancelled,
    /// UI 工具调用无效（如 `request_user_action` 无任何可交互问题）：
    /// 不能挂起等待用户（否则前端无卡片可交互会永久卡住），
    /// 由调用方闭合结束并把错误透出到 UI。
    Invalid { reason: String },
    Suspended { run_id: String },
}

/// 后端工具调用结果。
pub(super) enum BackendToolResult {
    Done,
    Cancelled,
}

/// 处理 UI 工具调用（如 request_user_action）。
pub(super) async fn handle_ui_tool_call<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
    call: &ToolCall,
    ui_strategy: &UIActionStrategy,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    queue: &mut VecDeque<Command>,
    // 本轮流式处理中记录的底层流错误（无则为空串），用于透出到 UI / 闭合文案。
    last_stream_error: &str,
) -> Result<UIActionOutcome> {
    let args: Value =
        serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| Value::Null);
    let message = args["message"].as_str().unwrap_or("").to_string();
    let questions = parse_ui_questions(&args["questions"]);

    // 空 questions：无任何可交互内容（常见于流式参数被截断/畸形 chunk 被吞，
    // 如 MiniMax 返回的 tool_calls 增量 `type:""` 导致含 arguments 的分片反序列化失败）。
    // 此时若照常挂起等待用户，前端没有卡片可作答，会话会永久卡在 awaiting，
    // 用户永远无法闭合。因此视为"无效调用"：闭合该 tool_call、把错误透出 UI 后结束。
    if questions.is_empty() {
        let mut reason =
            "request_user_action 未携带任何可交互的 questions（工具参数可能因流式解析失败而丢失）"
                .to_string();
        if !last_stream_error.is_empty() {
            reason.push_str("：");
            reason.push_str(last_stream_error);
        }
        warn!("{}", reason);
        close_tool_calls_with_reason(state, &[call.clone()], &reason);
        state
            .subscribers
            .emit(ChatEvent::Error(reason.clone()));
        return Ok(UIActionOutcome::Invalid { reason });
    }

    let run_id = state.config.lock().unwrap().run_id.clone();

    state
        .subscribers
        .emit(ChatEvent::Chat(CoreChatEvent::UIActionRequest {
            message,
            questions,
            session_id: run_id.clone(),
        }));

    match &ui_strategy {
        UIActionStrategy::BlockAndConfirm => {
            let confirmed = await_confirm(state, rx, queue, &call.id).await?;
            let Some((choice, action_id)) = confirmed else {
                return Ok(UIActionOutcome::UserCancelled);
            };
            // 用户选择结果作为 tool 消息写入，与其它工具一致
            let tool_content = serde_json::json!({
                "choice": choice,
                "action_id": action_id
            });
            state.history.push_tool(&call.id, &tool_content, ErrorType::None);
        }
        UIActionStrategy::EmitAndSuspend => {
            return Ok(UIActionOutcome::Suspended {
                run_id: run_id.unwrap_or_default(),
            });
        }
    }
    Ok(UIActionOutcome::Continue)
}

/// 执行后端工具调用。
pub(super) async fn execute_backend_tool_call<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
    call: &ToolCall,
    bridge: &dyn ToolExecutionBridge,
) -> Result<BackendToolResult> {
    let args: Value = serde_json::from_str(&call.function.arguments)
        .unwrap_or_else(|_| Value::String(call.function.arguments.clone()));
    info!(
        "[round] 执行工具: {} (id={}) args:{:?}",
        call.function.name, call.id, args
    );

    let outcome = if bridge.needs_stream(&call.function.name) {
        let (stream, handle) = bridge.create_stream(&call.function.name, &call.id);
        let result = state
            .tool_registry
            .call_tool_streamed(&call.function.name, args, &call.id, stream)
            .await;
        let _ = handle.await;
        result
    } else {
        state
            .tool_registry
            .call_tool(&call.function.name, args)
            .await
    };

    let (is_error, content) = match &outcome {
        Ok(o) => (o.result.is_error, o.result.content.clone()),
        Err(e) => {
            if state.cancelled.load(Ordering::SeqCst) {
                return Ok(BackendToolResult::Cancelled);
            }
            warn!("Tool '{}' failed: {}", call.function.name, e);
            (true, Value::String(format!("Error: {}", e)))
        }
    };

    info!(
        "[round] 工具 {} 执行完毕: is_error={}",
        call.function.name, is_error
    );
    let error_type = if is_error {
        ErrorType::ExecutionError
    } else {
        ErrorType::None
    };
    state.history.push_tool(&call.id, &content, error_type);
    state
        .subscribers
        .emit(ChatEvent::Chat(CoreChatEvent::ToolExecuted {
            id: call.id.clone(),
            name: call.function.name.clone(),
            is_error,
            content,
        }));

    Ok(BackendToolResult::Done)
}
