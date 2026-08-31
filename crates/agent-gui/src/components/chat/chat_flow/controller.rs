//! 业务流程编排 —— 发送消息、事件消费、用户操作回调。
//!
//! 所有函数接收 `&mut ChatSignals`（或 move 一份 Copy 副本），
//! 调用 `ChatSignals` 的纯数据方法完成状态变更。
//! 持久化由服务端 `History` + `ChatHistoryStore` 处理，GUI 不再自行持久化。

use std::sync::Arc;

use dioxus::prelude::{ReadableExt, WritableExt};

use planned_agent::chat::ChatEvent as ServiceChatEvent;
use planned_agent::ChatService;
use planned_agent_core::events::{ChatEvent, UIAction};
use planned_agent_prompt_manager::FilePromptManager;

use super::signals::ChatSignals;
use super::types::{AgentEvent, PendingUI};

// ── 发送消息 ──────────────────────────────────────────────────────────────

/// 发送消息：push user turn → send_text 入队。
///
/// 持久化由服务端 `History` + `ChatHistoryStore` 在 push 时自动完成，
/// GUI 不再自行持久化。
pub fn send_message(
    chat: &mut ChatSignals,
    svc: &Arc<ChatService<FilePromptManager>>,
    text: String,
) {
    chat.clear_pending();

    chat.push_user_turn(text.clone());

    if let Err(e) = svc.send_text(text) {
        chat.stop_streaming();
        chat.append_to_last_assistant(&format!("*发送失败: {}*", e));
    }
}

// ── 事件订阅 ──────────────────────────────────────────────────────────────

/// 注册一次事件订阅（guard 存入 subscription signal，drop 时自动退订）。
pub fn ensure_subscription(chat: &mut ChatSignals, chat_svc: &Arc<ChatService<FilePromptManager>>) {
    if chat.subscription.read().is_none() {
        let chat_copy = *chat;
        let guard = chat_svc.on_chat_with_guard(move |ev| handle_event(chat_copy, ev));
        chat.subscription.set(Some(guard));
    }
}

// ── 事件消费 ──────────────────────────────────────────────────────────────

/// 消费 `ChatEvent`：流式写入、交互卡片、子 agent UI、Done/Error 收尾。
pub fn handle_event(mut chat: ChatSignals, ev: ServiceChatEvent) {
    match ev {
        ServiceChatEvent::Chat(ChatEvent::TextDelta(chunk)) => {
            chat.append_streaming_text(&chunk);
        }
        ServiceChatEvent::Chat(ChatEvent::ReasoningDelta(chunk)) => {
            chat.append_streaming_reasoning(&chunk);
        }
        ServiceChatEvent::Chat(ChatEvent::RoundStart { .. }) => {
            let was_streaming = chat.is_streaming();
            tracing::info!(target: "event", event = "RoundStart", was_streaming, "RoundStart");
            if !was_streaming {
                chat.push_assistant_placeholder();
            }
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name, .. })
            if name == "request_user_action" =>
        {
            chat.pending_tool_call_id.set(Some(id));
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name, source }) => {
            let is_sub_agent = matches!(
                source,
                Some(planned_agent_core::tool_registry::types::ToolSource::SubAgent { .. })
            );
            tracing::info!(
                target: "event", event = "ToolCallStart",
                id = %id, name = %name, is_sub_agent, "ToolCallStart"
            );
            chat.tool_call_start(&id, &name, is_sub_agent);
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallArgsDelta { id, delta }) => {
            chat.tool_call_append_args(&id, &delta);
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallComplete {
            id,
            name,
            arguments,
        }) => {
            tracing::info!(target: "event", event = "ToolCallComplete", id = %id, name = %name, arguments = %arguments, "ToolCallComplete");
            if name != "request_user_action" {
                chat.tool_call_complete(&id, &name, &arguments);
            }
        }
        ServiceChatEvent::Chat(ChatEvent::ToolExecuted {
            id,
            name,
            is_error,
            content,
        }) => {
            tracing::info!(target: "event", event = "ToolExecuted", id = %id, name = %name, is_error, "ToolExecuted");
            chat.tool_call_executed(&id, &name, is_error, &content);
            // 子 agent 完成：更新 AgentView phase
            if chat.agent_views.read().contains_key(&id) {
                let phase = if is_error {
                    super::types::ToolCallPhase::Error
                } else {
                    super::types::ToolCallPhase::Completed
                };
                chat.finish_agent_view(&id, phase);
            }
        }
        ServiceChatEvent::Chat(ChatEvent::RoundEnd { .. }) => {
            tracing::info!(target: "event", event = "RoundEnd", "RoundEnd");
            chat.stop_streaming();
        }
        ServiceChatEvent::Chat(ChatEvent::UIActionRequest {
            message,
            actions,
            session_id,
        }) => {
            let tool_call_id = chat.pending_tool_call_id.read().clone().unwrap_or_default();
            tracing::info!(target: "event", event = "UIActionRequest", tool_call_id = %tool_call_id, session_id = ?session_id, message = %message, actions_count = actions.len(), "UIActionRequest");
            chat.set_pending(PendingUI {
                message,
                actions,
                tool_call_id,
                run_id: session_id,
            });
        }
        ServiceChatEvent::Chat(ChatEvent::SubChat { tool_call_id, event }) => {
            // 子 agent 流式事件：攒入对应 AgentViewData
            match *event {
                ChatEvent::TextDelta(ref text) => {
                    chat.push_agent_event(&tool_call_id, AgentEvent::TextDelta(text.clone()));
                }
                ChatEvent::ReasoningDelta(ref text) => {
                    chat.push_agent_event(&tool_call_id, AgentEvent::ReasoningDelta(text.clone()));
                }
                _ => {}
            }
        }
        ServiceChatEvent::Done { cancelled } => {
            tracing::info!(target: "event", event = "Done", cancelled, "Done");
            chat.stop_streaming();
            chat.finish_turn();
            chat.clear_pending();
            chat.pending_tool_call_id.set(None);
        }
        ServiceChatEvent::Error(e) => {
            tracing::error!(target: "event", event = "Error", error = %e, "聊天事件错误");
            // 后端已通过流式事件补了 error 消息，前端只做清理 + 闭合当前 turn
            chat.stop_streaming();
            chat.finish_turn();
            chat.clear_pending();
        }
        ServiceChatEvent::HistoryUpdated { messages } => {
            // 用快照校准 GUI 气泡（reconcile 保留 active 中正在 streaming 的气泡）
            tracing::info!(target: "event", event = "HistoryUpdated", count = messages.len(), "HistoryUpdated");
            // 保持注释（与现状一致）；启用时用 `chat.reconcile_with_snapshot(&messages)`。
        }
    }
}

// ── 用户操作回调 ──────────────────────────────────────────────────────────

/// 用户操作 `request_user_action` / 子 agent 挂起卡片后的回调。
pub fn handle_user_action(
    chat: &mut ChatSignals,
    svc: &Arc<ChatService<FilePromptManager>>,
    action: UIAction,
    choice: String,
    pending: PendingUI,
) {
    // 子 agent 场景：用户选择追加到 AgentViewData，而非父 agent 气泡
    if let Some(run_id) = pending.run_id.clone() {
        if chat.agent_views.read().contains_key(&run_id) {
            chat.push_agent_event(&run_id, AgentEvent::TextDelta(
                format!("\n\n---\n\n**{}**\n\n", choice)
            ));
        } else {
            // fallback：agent_views 里找不到（历史加载后），写到父 agent 气泡
            chat.append_to_last_assistant(&format!("\n\n---\n\n**{}**\n\n", choice));
        }
        chat.push_assistant_placeholder();
        chat.clear_pending();
        chat.pending_tool_call_id.set(None);
        let input = serde_json::json!({ "choice": choice, "action_id": action.id });
        if let Err(e) = svc.resume_sub_agent(&run_id, input) {
            chat.append_to_last_assistant(&format!("\n\n*子 agent 恢复出错: {}*", e));
            chat.stop_streaming();
        }
        return;
    }

    // 主 agent 场景：写到父 agent 气泡
    chat.append_to_last_assistant(&format!("\n\n---\n\n**{choice}**\n\n"));

    chat.push_assistant_placeholder();
    chat.clear_pending();
    chat.pending_tool_call_id.set(None);

    if let Err(e) = svc.confirm_user_action(&pending.tool_call_id, &choice, &action.id) {
        chat.append_to_last_assistant(&format!("\n\n*交互提交失败: {}*", e));
        chat.stop_streaming();
    }
}
