//! Plan 页面聊天流：发送消息、异步消费 ChatEvent、用户 UI 操作回调。
//!
//! 内部按"同步准备 → spawn 异步消费"分层；所有项以 `pub(super)` 对同级 `page` 暴露。

use dioxus::prelude::*;

use crate::services::chat_service::ChatServiceSignal;
use crate::storage::repository::MessageRepo;
use planned_agent::{ChatEvent, ChatService};
use planned_agent_core::types::{Message, MessageContent, MessageRole, UIAction};
use planned_agent_prompt_manager::FilePromptManager;
use std::sync::Arc;

use super::types::{
    display_text, display_text_mut, PendingUIState, PlanGeneratedEvent, PlanSource,
};

/// 把 Assistant 占位消息收敛为最终内容（关闭 streaming 光标）
pub(super) fn finalize_assistant(
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    content: &str,
) {
    if let Some(idx) = *streaming_idx.read() {
        if let Some(msg) = messages.write().get_mut(idx) {
            if let Some(t) = display_text_mut(msg) {
                *t = content.to_string();
            }
        }
    }
    streaming_idx.set(None);
}

/// 持久化一条消息到 DB（fire-and-forget）
fn persist_message(
    message_repo: &Arc<MessageRepo>,
    plan_id: &str,
    role: &str,
    content: &str,
) {
    let repo = message_repo.clone();
    let pid = plan_id.to_string();
    let r = role.to_string();
    let c = content.to_string();
    spawn(async move {
        if let Err(e) = repo.create(&pid, &r, &c).await {
            tracing::error!("持久化消息失败: {}", e);
        }
    });
}

// ─────────────────────────────────────────────────────────────────────
// 发送消息（顶层协调：同步准备 → spawn 异步消费）
// ─────────────────────────────────────────────────────────────────────

/// 同步入口：trim → push user/asst 占位 → 清输入 → 取 ChatService → spawn 异步流
pub(super) fn send_message(
    chat_signal: ChatServiceSignal,
    mut input_text: Signal<String, SyncStorage>,
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut reasoning_texts: Signal<Vec<Option<String>>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    mut pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
    plan_id: String,
    message_repo: Arc<MessageRepo>,
) {
    let text = input_text.read().trim().to_string();
    if text.is_empty() {
        return;
    }

    // 清除未响应的 UI action
    pending_ui.set(None);

    // 1. 推入 User 消息 + Assistant 占位
    let user_msg: Message = Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text { text: text.clone() }),
        ..Default::default()
    };
    let asst_msg = Message {
        role: MessageRole::Assistant,
        content: Some(MessageContent::Text { text: String::new() }),
        ..Default::default()
    };
    let asst_idx;
    {
        let mut msgs = messages.write();
        msgs.push(user_msg);
        reasoning_texts.write().push(None);
        asst_idx = msgs.len();
        msgs.push(asst_msg);
        reasoning_texts.write().push(Some(String::new()));
    }
    streaming_idx.set(Some(asst_idx));
    input_text.set(String::new());

    // 2. 持久化用户消息
    persist_message(&message_repo, &plan_id, "user", &text);

    // 3. 取 ChatService
    let chat = (*chat_signal.read()).clone();
    let Some(chat) = chat else {
        finalize_assistant(
            messages,
            streaming_idx,
            "AI/Tools 服务未就绪，无法发起聊天。",
        );
        return;
    };

    // 4. 转发到异步消费
    spawn(run_chat_stream(
        chat,
        messages,
        reasoning_texts,
        streaming_idx,
        pending_ui,
        plan_id,
        message_repo,
    ));
}

/// 异步消费 ChatEvent：实时写 signal 到 Dioxus runtime，流结束后持久化 assistant 消息。
async fn run_chat_stream(
    chat: Arc<ChatService<FilePromptManager>>,
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut reasoning_texts: Signal<Vec<Option<String> >, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    mut pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
    plan_id: String,
    message_repo: Arc<MessageRepo>,
) {
    let history: Vec<Message> = messages.read().clone();
    let result = chat
        .chat_with_callback(history, |event| match event {
            ChatEvent::TextDelta(chunk) => {
                if let Some(idx) = *streaming_idx.read() {
                    if let Some(msg) = messages.write().get_mut(idx) {
                        if let Some(t) = display_text_mut(msg) {
                            t.push_str(&chunk);
                        }
                    }
                }
            }
            ChatEvent::ReasoningDelta(chunk) => {
                if let Some(idx) = *streaming_idx.read() {
                    if let Some(Some(buf)) = reasoning_texts.write().get_mut(idx) {
                        buf.push_str(&chunk);
                    }
                }
            }
            ChatEvent::UIActionRequest { message, actions } => {
                *pending_ui.write() = Some(PendingUIState {
                    message,
                    actions,
                    history_snapshot: messages.read().clone(),
                });
            }
            _ => {}
        })
        .await;

    match result {
        Ok(response) if response.cancelled => {
            // 即使取消也持久化已生成的内容
            let sidx = *streaming_idx.read();
            if let Some(idx) = sidx {
                if let Some(msg) = messages.read().get(idx) {
                    let content = display_text(msg);
                    if !content.is_empty() {
                        persist_message(&message_repo, &plan_id, "assistant", content);
                    }
                }
            }
            streaming_idx.set(None);
        }
        Ok(response) => {
            streaming_idx.set(None);
            // 持久化 assistant 最终内容
            let msgs = messages.read();
            if let Some(last) = msgs.last() {
                if matches!(last.role, MessageRole::Assistant) {
                    let content = display_text(last);
                    if !content.is_empty() {
                        let repo = message_repo.clone();
                        let pid = plan_id.clone();
                        let c = content.to_string();
                        spawn(async move {
                            let _ = repo.create(&pid, "assistant", &c).await;
                        });
                    }
                }
            }
            let _ = response;
        }
        Err(e) => {
            tracing::error!("Plan: Chat 错误: {}", e);
            finalize_assistant(messages, streaming_idx, &format!("聊天失败: {}", e));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// 处理用户 UI 操作（点击按钮后的完整流程）
// ─────────────────────────────────────────────────────────────────────

pub(super) fn handle_user_action(
    action: UIAction,
    choice: String,
    pending: PendingUIState,
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut reasoning_texts: Signal<Vec<Option<String>>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    mut pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
    chat_signal: ChatServiceSignal,
    plan_mode: String,
    mut plan_generated: Signal<Option<PlanGeneratedEvent>, SyncStorage>,
) {
    let asst_idx = messages
        .read()
        .iter()
        .rposition(|m| matches!(m.role, MessageRole::Assistant));

    let asst_idx = match asst_idx {
        Some(i) => i,
        None => return,
    };

    // ── 路径 A：确认生成 → 提取计划文本 → 发出事件 → 终止 ──
    if action.id == "generate" {
        let plan_text = messages
            .read()
            .get(asst_idx)
            .map(|m| display_text(m).to_string())
            .unwrap_or_default();

        let source = match plan_mode.as_str() {
            "flexible" => PlanSource::Flexible,
            _ => PlanSource::Thorough,
        };

        plan_generated.set(Some(PlanGeneratedEvent { plan_text, source }));
        streaming_idx.set(None);
        pending_ui.set(None);
        return;
    }

    // ── 路径 B：其他动作（如 "edit"）→ 继续对话 ──
    if let Some(msg) = messages.write().get_mut(asst_idx) {
        if let Some(t) = display_text_mut(msg) {
            t.push_str(&format!("\n\n---\n\n**{}**\n\n", choice));
        }
    }

    streaming_idx.set(Some(asst_idx));
    pending_ui.set(None);

    let mut history = pending.history_snapshot;
    history.push(Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text {
            text: choice.clone(),
        }),
        ..Default::default()
    });

    let chat = (*chat_signal.read()).clone();
    let Some(chat) = chat else {
        if let Some(msg) = messages.write().get_mut(asst_idx) {
            if let Some(t) = display_text_mut(msg) {
                t.push_str("\n\n*AI 服务未就绪，无法继续对话。*");
            }
        }
        streaming_idx.set(None);
        return;
    };

    spawn(async move {
        let result = chat
            .chat_with_callback(history, |event| match event {
                ChatEvent::TextDelta(chunk) => {
                    if let Some(idx) = *streaming_idx.read() {
                        if let Some(msg) = messages.write().get_mut(idx) {
                            if let Some(t) = display_text_mut(msg) {
                                t.push_str(&chunk);
                            }
                        }
                    }
                }
                ChatEvent::ReasoningDelta(chunk) => {
                    if let Some(idx) = *streaming_idx.read() {
                        if let Some(Some(buf)) = reasoning_texts.write().get_mut(idx) {
                            buf.push_str(&chunk);
                        }
                    }
                }
                ChatEvent::UIActionRequest { message, actions } => {
                    *pending_ui.write() = Some(PendingUIState {
                        message,
                        actions,
                        history_snapshot: messages.read().clone(),
                    });
                }
                _ => {}
            })
            .await;

        match result {
            Ok(response) => {
                streaming_idx.set(None);
                let _ = response;
            }
            Err(e) => {
                tracing::error!("Plan: handle_user_action Chat 错误: {}", e);
                if let Some(idx) = *streaming_idx.read() {
                    if let Some(msg) = messages.write().get_mut(idx) {
                        if let Some(t) = display_text_mut(msg) {
                            t.push_str(&format!("\n\n*出错: {}*", e));
                        }
                    }
                }
                streaming_idx.set(None);
            }
        }
    });
}
