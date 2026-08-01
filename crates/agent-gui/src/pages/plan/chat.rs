//! Plan 页面聊天流：发送消息、异步消费 ChatEvent、用户 UI 操作回调。
//!
//! 内部按"同步准备 → spawn 异步消费"分层；所有项以 `pub(super)` 对同级 `page` 暴露。

use dioxus::prelude::*;

use crate::services::chat_service::ChatServiceSignal;
use planned_agent::{ChatEvent, ChatService};
use planned_agent_core::types::{Message, MessageContent, MessageRole, UIAction};
use planned_agent_prompt_manager::FilePromptManager;
use std::sync::Arc;

use super::types::{display_text, display_text_mut, PendingUIState};

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

// ─────────────────────────────────────────────────────────────────────
// 发送消息（顶层协调：同步准备 → spawn 异步消费）
// ─────────────────────────────────────────────────────────────────────

/// 同步入口：trim → push user/asst 占位 → 清输入 → 取 ChatService → spawn 异步流
///
/// 关注点分离：
/// - 本函数：所有 *同步* 的 UI signal 副作用（在 Dioxus runtime 上下文里）
/// - `run_chat_stream`：异步消费 ChatEvent，可独立演化
pub(super) fn send_message(
    chat_signal: ChatServiceSignal,
    mut input_text: Signal<String, SyncStorage>,
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    mut pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
) {
    let text = input_text.read().trim().to_string();
    if text.is_empty() {
        return;
    }

    // 清除未响应的 UI action（用户选择了直接输入文本而非点击按钮）
    pending_ui.set(None);

    // 1. 推入 User 消息 + Assistant 占位，并记录 Assistant 的下标
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
        asst_idx = msgs.len();
        msgs.push(asst_msg);
    }
    streaming_idx.set(Some(asst_idx));
    input_text.set(String::new());

    // 2. 取 ChatService（未就绪时直接 finalize 并返回）
    let chat = (*chat_signal.read()).clone();
    let Some(chat) = chat else {
        finalize_assistant(
            messages,
            streaming_idx,
            "AI/Tools 服务未就绪，无法发起聊天。",
        );
        return;
    };

    // 3. 转发到异步消费（在 spawn 里把 messages / streaming_idx / pending_ui 移交给 future）
    spawn(run_chat_stream(chat, messages, streaming_idx, pending_ui));
}

/// 异步消费 ChatEvent：实时写 signal 到 Dioxus runtime，立即 yield。
///
/// - `history` 在调用前 snapshot（一次性克隆当前 messages 列表）
/// - 消费 `TextDelta`（追加分词）和 `UIActionRequest`（设置 pending_ui）
/// - 终态收敛：有 pending_ui_actions → 关闭 streaming 光标；无 → 用 response.message 覆盖
async fn run_chat_stream(
    chat: Arc<ChatService<FilePromptManager>>,
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    mut pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
) {
    let history: Vec<Message> = messages.read().clone();
    let result = chat
        .chat_with_callback(history, |event| match event {
            ChatEvent::TextDelta(chunk) => {
                // 实时追加 chunk 到当前 streaming 的 Assistant 占位
                if let Some(idx) = *streaming_idx.read() {
                    if let Some(msg) = messages.write().get_mut(idx) {
                        if let Some(t) = display_text_mut(msg) {
                            t.push_str(&chunk);
                        }
                    }
                }
            }
            ChatEvent::UIActionRequest { message, actions } => {
                // Agent 请求用户交互：保存状态供前端渲染按钮
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
            if !response.pending_ui_actions.is_empty() {
                // UI actions 已通过 event 设置到 pending_ui signal
                // 清除 streaming 光标，按钮会在消息区下方渲染
                streaming_idx.set(None);
            } else {
                let final_text = display_text(&response.message).to_string();
                finalize_assistant(messages, streaming_idx, &final_text);
            }
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

/// 用户点击 UI action 按钮后的处理：
/// 1. 替换占位 tool result 为真实用户选择
/// 2. 更新 messages → 追加 Assistant 占位
/// 3. 继续 chat_with_callback 获取 LLM 后续响应
pub(super) fn handle_user_action(
    action: UIAction,
    pending: PendingUIState,
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    mut pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
    chat_signal: ChatServiceSignal,
) {
    // 1. 获取历史快照
    let mut history = pending.history_snapshot;

    // 2. 替换占位 tool result 为真实的用户选择
    for msg in history.iter_mut().rev() {
        if let Some(MessageContent::ToolResult { content, .. }) = &mut msg.content {
            if content.contains("awaiting_user_input") {
                *content = serde_json::to_string(&serde_json::json!({
                    "action_id": action.id,
                    "action_type": serde_json::to_value(&action.action_type).unwrap_or_default(),
                    "choice": action.label,
                }))
                .unwrap_or_default();
                break;
            }
        }
    }

    // 3. 更新 UI messages（展示用户选择）
    messages.set(history.clone());

    // 4. 清除 pending 状态
    pending_ui.set(None);

    // 5. 添加新的 assistant 占位（用于流式输出）
    let asst_idx;
    {
        let mut msgs = messages.write();
        asst_idx = msgs.len();
        msgs.push(Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text {
                text: String::new(),
            }),
            ..Default::default()
        });
    }
    streaming_idx.set(Some(asst_idx));

    // 6. 继续聊天
    let chat = (*chat_signal.read()).clone();
    let Some(chat) = chat else {
        finalize_assistant(
            messages,
            streaming_idx,
            "AI 服务未就绪，无法继续对话。",
        );
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
                if response.pending_ui_actions.is_empty() {
                    finalize_assistant(
                        messages,
                        streaming_idx,
                        &display_text(&response.message),
                    );
                } else {
                    streaming_idx.set(None);
                }
            }
            Err(e) => {
                tracing::error!("Plan: handle_user_action Chat 错误: {}", e);
                finalize_assistant(
                    messages,
                    streaming_idx,
                    &format!("出错: {}", e),
                );
            }
        }
    });
}
