//! 业务流程编排 —— 发送消息、事件消费、用户操作回调。
//!
//! 所有函数接收 `&mut ChatSignals`（或 move 一份 Copy 副本），
//! 调用 `ChatSignals` 的纯数据方法完成状态变更。
//! 依赖 `ChatService`、`ChatStorage` 等外部 crate。

use std::sync::Arc;

use dioxus::prelude::{ReadableExt, WritableExt};

use planned_agent::chat::ChatEvent as ServiceChatEvent;
use planned_agent::ChatService;
use planned_agent_core::events::{ChatEvent, UIAction};
use planned_agent_prompt_manager::FilePromptManager;

use super::signals::ChatSignals;
use super::types::{ChatMessage, PendingUI};
use crate::services::chat_service::ChatServiceSignal;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

// ── 内部辅助 ──────────────────────────────────────────────────────────────

/// 持久化一条消息（fire-and-forget）。
fn spawn_persist(chat: &ChatSignals, cm: ChatMessage) {
    let storage = chat.ctx.read().storage.clone();
    let pid = chat.ctx.read().plan_id.clone();
    tracing::info!(
        target: "persist",
        role = ?cm.message.role,
        seq = cm.sequence_order,
        is_streaming = cm.is_streaming,
        tool_entries = cm.tool_call_entries.len(),
        "spawn_persist"
    );
    tokio::spawn(async move {
        storage.persist_message(&pid, &cm).await;
    });
}

/// 增量持久化：将 `seq > last_persisted_seq` 的新增消息写入存储并推进游标。
///
/// 在 RoundEnd / Error / Done 时调用，保证即使一轮没有正常 RoundEnd
/// （流式错误、用户停止），已产生的消息也不会因游标前移而永久丢失。
fn persist_incremental(chat: &mut ChatSignals) {
    let persisted_upto = *chat.last_persisted_seq.read();
    let new_msgs: Vec<ChatMessage> = chat
        .messages
        .read()
        .iter()
        .filter(|m| m.sequence_order > persisted_upto)
        .cloned()
        .collect();
    for cm in &new_msgs {
        tracing::info!(target: "persist", event = "incremental", role = ?cm.message.role, seq = cm.sequence_order, "增量持久化");
        spawn_persist(chat, cm.clone());
    }
    let max_seq = chat
        .messages
        .read()
        .iter()
        .map(|m| m.sequence_order)
        .max()
        .unwrap_or(0);
    chat.last_persisted_seq.set(max_seq);
}

// ── 发送消息 ──────────────────────────────────────────────────────────────

/// 发送消息：push user turn → 持久化 → send_text 入队。
pub fn send_message(chat: &mut ChatSignals, chat_service_signal: ChatServiceSignal, text: String) {
    chat.clear_pending();

    let seq_start = chat
        .messages
        .read()
        .iter()
        .map(|m| m.sequence_order)
        .max()
        .unwrap_or(0)
        + 1;
    let mut seq = seq_start;

    // ── 持久化 user 消息（在 push_user_turn 之前快照） ──
    let user_msg_snapshot = {
        let user_msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text: text.clone() }),
            ..Default::default()
        };
        ChatMessage {
            message: user_msg,
            sequence_order: seq,
            is_streaming: false,
            tool_call_id: None,
            tool_call_entries: Vec::new(),
        }
    };
    spawn_persist(chat, user_msg_snapshot);
    // 注意：此处不推进 last_persisted_seq —— 游标统一由
    // persist_incremental（RoundEnd/Error/Done）推进。user 消息若
    // fire-and-forget 写失败，会被后续增量重写兜底（存储层幂等跳过已写入的）。

    chat.push_user_turn(text.clone(), &mut seq);

    let chat_svc = (*chat_service_signal.read()).clone();
    let Some(chat_svc) = chat_svc else {
        chat.stop_streaming();
        chat.append_to_last_assistant("*AI/Tools 服务未就绪，无法发起聊天。*");
        return;
    };

    if let Err(e) = chat_svc.send_text(text) {
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
            chat.append_streaming(&chunk);
        }
        ServiceChatEvent::Chat(ChatEvent::ReasoningDelta(chunk)) => {
            chat.append_streaming_reasoning(&chunk);
        }
        ServiceChatEvent::Chat(ChatEvent::RoundStart { .. }) => {
            let was_streaming = chat.is_streaming();
            tracing::info!(target: "event", event = "RoundStart", was_streaming, "RoundStart");
            if !was_streaming {
                let seq = chat
                    .messages
                    .read()
                    .iter()
                    .map(|m| m.sequence_order)
                    .max()
                    .unwrap_or(0)
                    + 1;
                let mut seq = seq;
                chat.push_assistant_placeholder(&mut seq);
            }
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name })
            if name == "request_user_action" =>
        {
            chat.pending_tool_call_id.set(Some(id));
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name }) => {
            tracing::info!(target: "event", event = "ToolCallStart", id = %id, name = %name, "ToolCallStart");
            chat.tool_call_start(&id, &name);
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
            // tool_call_executed 内部已创建 Tool 消息，无需在此 persist
            chat.tool_call_executed(&id, &name, is_error, &content);
        }
        ServiceChatEvent::Chat(ChatEvent::RoundEnd { .. }) => {
            // 增量持久化：只写 seq > last_persisted_seq 的本轮新增消息
            // （user 消息已在 send_message 时提交；此处覆盖 assistant + tool 消息），
            // 避免每轮全量重放造成的重复写入与并发竞态
            persist_incremental(&mut chat);
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
        ServiceChatEvent::Done { cancelled } => {
            tracing::info!(target: "event", event = "Done", cancelled, "Done");
            chat.stop_streaming();
            chat.clear_pending();
            chat.pending_tool_call_id.set(None);
            // 用户停止等场景无 RoundEnd，收尾时补齐未持久化的消息
            persist_incremental(&mut chat);
        }
        ServiceChatEvent::Error(e) => {
            tracing::error!(target: "event", event = "Error", error = %e, "聊天事件错误");
            if chat.pending_ui.read().is_none() {
                chat.stop_streaming();
                chat.append_to_last_assistant(&format!("\n\n*聊天出错: {}*", e));
                // 流式错误无 RoundEnd，补齐已产生的消息避免游标前移后永久丢失
                persist_incremental(&mut chat);
            }
        }
    }
}

// ── 用户操作回调 ──────────────────────────────────────────────────────────

/// 用户操作 `request_user_action` / 子 agent 挂起卡片后的回调。
pub fn handle_user_action(
    chat: &mut ChatSignals,
    chat_service_signal: ChatServiceSignal,
    action: UIAction,
    choice: String,
    pending: PendingUI,
) {
    if let Some(asst_idx) = chat.last_assistant_idx() {
        chat.append_text(asst_idx, &format!("\n\n---\n\n**{choice}**\n\n"));
    }

    let seq = chat
        .messages
        .read()
        .iter()
        .map(|m| m.sequence_order)
        .max()
        .unwrap_or(0)
        + 1;
    let mut seq = seq;
    chat.push_assistant_placeholder(&mut seq);
    chat.clear_pending();
    chat.pending_tool_call_id.set(None);

    let chat_svc = (*chat_service_signal.read()).clone();
    let Some(chat_svc) = chat_svc else {
        chat.append_to_last_assistant("\n\n*AI 服务未就绪，无法继续对话。*");
        chat.stop_streaming();
        return;
    };

    if let Some(run_id) = pending.run_id.clone() {
        let input = serde_json::json!({ "choice": choice, "action_id": action.id });
        if let Err(e) = chat_svc.resume_sub_agent(&run_id, input) {
            chat.append_to_last_assistant(&format!("\n\n*子 agent 恢复出错: {}*", e));
            chat.stop_streaming();
        }
        return;
    }

    if let Err(e) = chat_svc.confirm_user_action(&pending.tool_call_id, &choice, &action.id) {
        chat.append_to_last_assistant(&format!("\n\n*交互提交失败: {}*", e));
        chat.stop_streaming();
    }
}
