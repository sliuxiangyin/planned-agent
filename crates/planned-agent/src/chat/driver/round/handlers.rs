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
use super::UIActionStrategy;
use crate::chat::service::ChatEvent;
use crate::chat::state::{Command, State};
use crate::chat::storage::ErrorType;
use crate::chat::tools::parse_ui_actions;

/// UI 工具调用结果。
pub(super) enum UIActionOutcome {
    Continue,
    UserCancelled,
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
) -> Result<UIActionOutcome> {
    let args: Value =
        serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| Value::Null);
    let message = args["message"].as_str().unwrap_or("").to_string();
    let actions = parse_ui_actions(&args["actions"]);
    let run_id = state.config.lock().unwrap().run_id.clone();

    state
        .subscribers
        .emit(ChatEvent::Chat(CoreChatEvent::UIActionRequest {
            message,
            actions,
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
