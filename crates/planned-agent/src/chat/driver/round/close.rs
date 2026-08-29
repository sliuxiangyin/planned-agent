//! 历史闭合函数：中断后闭合 tool_calls、补齐孤立消息。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::events::ChatEvent as CoreChatEvent;
use tracing::info;

use crate::chat::service::ChatEvent;
use crate::chat::state::State;

/// 中断后闭合：确保最后一条 assistant 消息的所有 tool_calls 都有对应的 tool 消息。
pub(in crate::chat::driver) fn close_unclosed_tool_calls<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
) {
    if !state.cancelled.load(Ordering::SeqCst) {
        return;
    }
    let messages = state.history.snapshot();
    if let Some(last_msg) = messages.last() {
        if matches!(last_msg.role, MessageRole::Assistant) {
            if let Some(tool_calls) = &last_msg.tool_calls {
                if !tool_calls.is_empty() {
                    info!(
                        "[round] 中断后闭合：最后一条 assistant 消息包含 {} 个 tool_calls，写入 cancelled tool 消息",
                        tool_calls.len()
                    );
                    close_tool_calls_with_reason(state, tool_calls, "任务被中断");
                }
            }
        }
    }
}

/// max_tool_rounds 场景：为最后一条 assistant 的 tool_calls 补 cancelled tool 消息。
pub(super) fn close_max_rounds_tool_calls<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
) {
    let messages = state.history.snapshot();
    if let Some(last_msg) = messages.last() {
        if matches!(last_msg.role, MessageRole::Assistant) {
            if let Some(tool_calls) = &last_msg.tool_calls {
                if !tool_calls.is_empty() {
                    info!(
                        "[round] 达到 max_tool_rounds：为 {} 个 tool_calls 写入 cancelled tool 消息",
                        tool_calls.len()
                    );
                    close_tool_calls_with_reason(state, tool_calls, "达到最大轮次限制");
                }
            }
        }
    }
}

/// 为 tool_calls 列表补 cancelled tool 消息并 emit ToolExecuted 事件。
pub(super) fn close_tool_calls_with_reason<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
    tool_calls: &[planned_agent_core::ai::types::ToolCall],
    reason: &str,
) {
    for tc in tool_calls {
        state.history.push_cancelled_tool(&tc.id, reason);
        state.subscribers.emit(ChatEvent::Chat(CoreChatEvent::ToolExecuted {
            id: tc.id.clone(),
            name: tc.function.name.clone(),
            is_error: true,
            content: serde_json::Value::String(reason.to_string()),
        }));
    }
}

/// 判断 tool 消息是否是 cancelled（由 close_unclosed_tool_calls 生成）。
fn is_cancelled_tool(msg: &Message) -> bool {
    if let Some(MessageContent::ToolResult { content, .. }) = &msg.content {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(content) {
            return json.get("cancelled").and_then(|v| v.as_bool()).unwrap_or(false);
        }
    }
    false
}

/// 补齐孤立消息：若最后一条是 User 或执行成功的 Tool，补一条 assistant 消息并 emit 流式事件。
///
/// 用于以下场景：
/// - 中断后：最后一条是 User，补 "Output interrupt"
/// - 中断后：最后一条是执行成功的 Tool，补 "Output interrupt"（避免 LLM 困惑）
/// - Error 后：最后一条是 User，补 "Error: xxx"
///
/// 不补的场景：
/// - 最后一条是 cancelled Tool（close_unclosed_tool_calls 已处理）
/// - 最后一条是执行失败的 Tool（已告知失败）
/// - 最后一条是 Assistant（不需要补）
pub(in crate::chat::driver) fn close_orphaned_user<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
    text: &str,
) {
    let messages = state.history.snapshot();
    if let Some(last) = messages.last() {
        let should_close = match last.role {
            // 最后一条是 User → LLM 未及回复，需要补
            MessageRole::User => true,
            // 最后一条是 Tool → 检查是否需要补
            MessageRole::Tool => {
                if is_cancelled_tool(last) {
                    // cancelled tool → close_unclosed_tool_calls 已处理，跳过
                    false
                } else {
                    // 执行成功或普通失败的 tool → 需要补 assistant 给 LLM 上下文
                    true
                }
            }
            // Assistant 或 System → 不需要补
            _ => false,
        };

        if should_close {
            info!("[round] 补齐孤立消息：最后一条是 {:?}，写入 assistant", last.role);
            let msg = Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Text {
                    text: text.to_string(),
                }),
                ..Default::default()
            };
            state.history.push_assistant(msg.clone());
            // 推送流式事件组合，让 GUI 同步
            state.subscribers.emit(ChatEvent::Chat(CoreChatEvent::RoundStart { round: 1 }));
            state.subscribers.emit(ChatEvent::Chat(CoreChatEvent::TextDelta(text.to_string())));
            state.subscribers.emit(ChatEvent::Chat(CoreChatEvent::RoundEnd { message: msg }));
        }
    }
}
