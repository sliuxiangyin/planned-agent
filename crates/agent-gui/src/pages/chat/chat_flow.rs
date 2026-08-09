//! Chat 测试页：聊天消息流转逻辑（纯内存，不持久化）。
//!
//! 结构上参考 `pages/plan/chat.rs`，但去掉 plan 专属逻辑（计划生成事件、
//! 参数固化、消息落库），只保留：发送 → 流式消费 → `request_user_action`
//! 卡片 → 用户操作回传 → 继续对话 的闭环。

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::chat::UIAction;
use planned_agent::{ChatEvent, ChatService};
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_prompt_manager::FilePromptManager;

use crate::services::chat_service::ChatServiceSignal;

/// 待处理的 UI 交互（`request_user_action` 卡片状态）。
#[derive(Clone)]
pub(crate) struct PendingUI {
    /// 展示给用户的引导文本
    pub(crate) message: String,
    /// 用户可选的动作列表
    pub(crate) actions: Vec<UIAction>,
    /// 当时的对话历史快照（用户操作后用于继续 chat）
    pub(crate) history_snapshot: Vec<Message>,
}

/// Chat 测试页的消息状态（纯内存，Signal 均为 `Copy` 可直接进闭包/异步块）。
#[derive(Clone, Copy)]
pub(crate) struct ChatSignals {
    pub(crate) messages: Signal<Vec<Message>, SyncStorage>,
    pub(crate) reasoning_texts: Signal<Vec<Option<String>>, SyncStorage>,
    pub(crate) streaming_idx: Signal<Option<usize>, SyncStorage>,
    pub(crate) pending_ui: Signal<Option<PendingUI>, SyncStorage>,
    pub(crate) input_text: Signal<String, SyncStorage>,
}

impl ChatSignals {
    /// 当前 streaming 消息索引快照。
    pub(crate) fn sidx(&self) -> Option<usize> {
        *self.streaming_idx.read()
    }

    /// 最后一条 Assistant 消息的索引。
    pub(crate) fn last_assistant_idx(&self) -> Option<usize> {
        self.messages
            .read()
            .iter()
            .rposition(|m| matches!(m.role, MessageRole::Assistant))
    }

    /// 推入用户消息 + Assistant 占位，对齐 `reasoning_texts`，返回 assistant 索引。
    pub(crate) fn push_user_turn(&mut self, user_text: String) -> usize {
        let user_msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text: user_text }),
            ..Default::default()
        };
        let asst_msg = Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text { text: String::new() }),
            ..Default::default()
        };
        let asst_idx;
        {
            let mut msgs = self.messages.write();
            msgs.push(user_msg);
            self.reasoning_texts.write().push(None);
            asst_idx = msgs.len();
            msgs.push(asst_msg);
            self.reasoning_texts.write().push(Some(String::new()));
        }
        self.streaming_idx.set(Some(asst_idx));
        asst_idx
    }

    /// 向指定索引的消息追加文本（流式 delta）。
    pub(crate) fn append_text(&mut self, idx: usize, chunk: &str) {
        if let Some(msg) = self.messages.write().get_mut(idx) {
            if let Some(MessageContent::Text { text }) = &mut msg.content {
                text.push_str(chunk);
            }
        }
    }

    /// 向当前 streaming 消息追加文本。
    pub(crate) fn append_streaming(&mut self, chunk: &str) {
        if let Some(idx) = self.sidx() {
            self.append_text(idx, chunk);
        }
    }

    /// 向指定索引追加推理文本。
    pub(crate) fn append_reasoning(&mut self, idx: usize, chunk: &str) {
        if let Some(Some(buf)) = self.reasoning_texts.write().get_mut(idx) {
            buf.push_str(chunk);
        }
    }

    /// 向当前 streaming 消息追加推理文本。
    pub(crate) fn append_streaming_reasoning(&mut self, chunk: &str) {
        if let Some(idx) = self.sidx() {
            self.append_reasoning(idx, chunk);
        }
    }

    /// 将指定索引消息文本替换为最终内容并停止 streaming。
    pub(crate) fn finalize_at(&mut self, idx: usize, content: &str) {
        if let Some(msg) = self.messages.write().get_mut(idx) {
            if let Some(MessageContent::Text { text }) = &mut msg.content {
                *text = content.to_string();
            }
        }
        self.streaming_idx.set(None);
    }

    /// 停止 streaming（保留当前内容）。
    pub(crate) fn stop_streaming(&mut self) {
        self.streaming_idx.set(None);
    }

    /// 向最后一条 Assistant 消息追加文本。
    pub(crate) fn append_to_last_assistant(&mut self, text: &str) {
        if let Some(idx) = self.last_assistant_idx() {
            self.append_text(idx, text);
        }
    }

    /// 设置待处理 UI 交互。
    pub(crate) fn set_pending(&mut self, state: PendingUI) {
        *self.pending_ui.write() = Some(state);
    }

    /// 清除待处理 UI 交互。
    pub(crate) fn clear_pending(&mut self) {
        self.pending_ui.set(None);
    }

    /// 清空全部消息与相关状态（内存级别）。
    pub(crate) fn clear(&mut self) {
        self.messages.set(vec![]);
        self.reasoning_texts.set(vec![]);
        self.streaming_idx.set(None);
        self.pending_ui.set(None);
    }
}

/// 发送消息：同步准备（push user + assistant 占位）→ spawn 异步消费事件流。
pub(crate) fn send_message(
    chat_signal: ChatServiceSignal,
    mut chat: ChatSignals,
    text: String,
) {
    chat.clear_pending();
    chat.push_user_turn(text);

    let chat_svc = (*chat_signal.read()).clone();
    let Some(chat_svc) = chat_svc else {
        chat.finalize_at(chat.sidx().unwrap_or(0), "AI/Tools 服务未就绪，无法发起聊天。");
        return;
    };

    spawn(run_chat_stream(chat_svc, chat));
}

/// 异步消费 ChatEvent：文本/推理流式写入，`request_user_action` 转为内嵌卡片。
async fn run_chat_stream(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut chat: ChatSignals,
) {
    let history: Vec<Message> = chat.messages.read().clone();

    let result = chat_svc
        .chat_with_callback(history, |event| match event {
            ChatEvent::TextDelta(chunk) => {
                chat.append_streaming(&chunk);
            }
            ChatEvent::ReasoningDelta(chunk) => {
                chat.append_streaming_reasoning(&chunk);
            }
            ChatEvent::UIActionRequest { message, actions } => {
                let snapshot = chat.messages.read().clone();
                chat.set_pending(PendingUI {
                    message,
                    actions,
                    history_snapshot: snapshot,
                });
            }
            _ => {}
        })
        .await;

    match result {
        Ok(_response) => {
            chat.stop_streaming();
        }
        Err(e) => {
            tracing::error!("Chat 测试页聊天失败: {}", e);
            chat.stop_streaming();
            chat.append_to_last_assistant(&format!("\n\n*聊天出错: {}*", e));
        }
    }
}

/// 用户操作 `request_user_action` 卡片后的回调：把 choice 作为 user 消息
/// 写入历史，继续与 AI 对话。
pub(crate) fn handle_user_action(
    _action: UIAction,
    choice: String,
    pending: PendingUI,
    mut chat: ChatSignals,
    chat_signal: ChatServiceSignal,
) {
    let Some(asst_idx) = chat.last_assistant_idx() else {
        return;
    };

    // 在消息气泡中回显用户的选择，并清掉卡片、恢复流式光标
    chat.append_to_last_assistant(&format!("\n\n---\n\n**{choice}**\n\n"));
    chat.streaming_idx.set(Some(asst_idx));
    chat.clear_pending();

    let mut history = pending.history_snapshot;
    history.push(Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text { text: choice }),
        ..Default::default()
    });

    let chat_svc = (*chat_signal.read()).clone();
    let Some(chat_svc) = chat_svc else {
        chat.append_to_last_assistant("\n\n*AI 服务未就绪，无法继续对话。*");
        chat.stop_streaming();
        return;
    };

    spawn(async move {
        let result = chat_svc
            .chat_with_callback(history, |event| match event {
                ChatEvent::TextDelta(chunk) => {
                    chat.append_streaming(&chunk);
                }
                ChatEvent::ReasoningDelta(chunk) => {
                    chat.append_streaming_reasoning(&chunk);
                }
                ChatEvent::UIActionRequest { message, actions } => {
                    let snapshot = chat.messages.read().clone();
                    chat.set_pending(PendingUI {
                        message,
                        actions,
                        history_snapshot: snapshot,
                    });
                }
                _ => {}
            })
            .await;

        match result {
            Ok(_response) => {
                chat.stop_streaming();
            }
            Err(e) => {
                tracing::error!("Chat 测试页继续对话失败: {}", e);
                chat.stop_streaming();
                chat.append_to_last_assistant(&format!("\n\n*交互出错: {}*", e));
            }
        }
    });
}
