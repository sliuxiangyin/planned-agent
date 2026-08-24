//! 聊天信号容器 —— `ChatSignals` 结构体及其操作方法。
//!
//! 纯内存状态管理，`Signal` 均为 `Copy` 可直接进闭包/异步块。
//! `ChatContext`（storage + plan_id）通过 `Signal` 嵌入，方法内部访问。

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::chat::{ChatEvent as ServiceChatEvent, SubscriptionGuard};
use planned_agent::ChatService;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::events::{ChatEvent, UIAction};
use planned_agent_prompt_manager::FilePromptManager;

use super::types::{ChatContext, ChatMessage, PendingUI, ToolCallEntry, ToolCallPhase};
use crate::services::chat_service::ChatServiceSignal;

/// 消息状态（纯内存，Signal 均为 `Copy` 可直接进闭包/异步块）。
#[derive(Clone, Copy, PartialEq)]
pub struct ChatSignals {
    pub messages: Signal<Vec<ChatMessage>, SyncStorage>,
    pub pending_ui: Signal<Option<PendingUI>, SyncStorage>,
    pub input_text: Signal<String, SyncStorage>,
    /// 最近一次 `request_user_action` tool call 的 id
    pub pending_tool_call_id: Signal<Option<String>, SyncStorage>,
    /// 当前 service 上已注册的事件订阅守卫（RAII 自动退订）
    pub subscription: Signal<Option<SubscriptionGuard>, SyncStorage>,
    /// 会话上下文（storage + plan_id），初始化后只读。
    /// 用 Signal 包裹是因为 ChatSignals 需要 Copy（Dioxus rsx! 要求）。
    pub ctx: Signal<ChatContext, SyncStorage>,
}

// ── 状态查询 ──────────────────────────────────────────────────────────────

impl ChatSignals {
    pub(crate) fn current_streaming(&self) -> Option<ChatMessage> {
        self.messages
            .read()
            .iter()
            .rev()
            .find(|m| m.is_streaming)
            .cloned()
    }

    pub fn is_streaming(&self) -> bool {
        self.messages.read().iter().any(|m| m.is_streaming)
    }

    pub fn last_assistant_idx(&self) -> Option<usize> {
        self.messages
            .read()
            .iter()
            .rposition(|m| matches!(m.message.role, MessageRole::Assistant))
    }

    pub fn has_pending(&self) -> bool {
        self.pending_ui.read().is_some()
    }
}

// ── 消息操作 ──────────────────────────────────────────────────────────────

impl ChatSignals {
    pub fn push_user_turn(&mut self, user_text: String, seq: &mut u64) {
        let user_msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text: user_text }),
            ..Default::default()
        };
        self.messages.write().push(ChatMessage {
            message: user_msg,
            sequence_order: *seq,
            is_streaming: false,
            tool_call_entries: Vec::new(),
        });
        *seq += 1;
        self.push_assistant_placeholder(seq);
    }

    pub fn push_assistant_placeholder(&mut self, seq: &mut u64) {
        let asst_msg = Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text { text: String::new() }),
            ..Default::default()
        };
        self.messages.write().push(ChatMessage {
            message: asst_msg,
            sequence_order: *seq,
            is_streaming: true,
            tool_call_entries: Vec::new(),
        });
        *seq += 1;
    }

    pub fn append_text(&mut self, idx: usize, chunk: &str) {
        if let Some(cm) = self.messages.write().get_mut(idx) {
            if let Some(MessageContent::Text { text }) = &mut cm.message.content {
                text.push_str(chunk);
            }
        }
    }

    pub fn append_streaming(&mut self, chunk: &str) {
        if let Some(cm) = self.messages.write().iter_mut().find(|m| m.is_streaming) {
            if let Some(MessageContent::Text { text }) = &mut cm.message.content {
                text.push_str(chunk);
            }
        }
    }

    pub fn append_streaming_reasoning(&mut self, chunk: &str) {
        if let Some(cm) = self.messages.write().iter_mut().find(|m| m.is_streaming) {
            let buf = cm.message.reasoning_content.get_or_insert_with(String::new);
            buf.push_str(chunk);
        }
    }

    pub fn stop_streaming(&mut self) {
        for cm in self.messages.write().iter_mut() {
            if cm.is_streaming {
                cm.is_streaming = false;
            }
        }
    }

    pub fn append_to_last_assistant(&mut self, text: &str) {
        if let Some(idx) = self.last_assistant_idx() {
            self.append_text(idx, text);
        }
    }

    pub fn set_pending(&mut self, state: PendingUI) {
        *self.pending_ui.write() = Some(state);
    }

    pub fn clear_pending(&mut self) {
        self.pending_ui.set(None);
    }

    pub fn clear(&mut self) {
        self.messages.set(vec![]);
        self.pending_ui.set(None);
        self.pending_tool_call_id.set(None);
    }
}

// ── Tool 调用管理 ─────────────────────────────────────────────────────────

impl ChatSignals {
    pub fn tool_call_start(&mut self, _id: &str, name: &str) {
        if let Some(cm) = self.messages.write().iter_mut().find(|m| m.is_streaming) {
            cm.tool_call_entries.push(ToolCallEntry {
                name: name.to_string(),
                phase: ToolCallPhase::Pending,
                arguments: String::new(),
                result: None,
                is_error: false,
            });
        }
    }

    pub fn tool_call_append_args(&mut self, id: &str, delta: &str) {
        if let Some(cm) = self.messages.write().iter_mut().find(|m| m.is_streaming) {
            if let Some(entry) = cm
                .tool_call_entries
                .iter_mut()
                .rfind(|e| e.name == id || e.arguments.is_empty())
            {
                entry.arguments.push_str(delta);
            }
        }
    }

    pub fn tool_call_complete(&mut self, _id: &str, name: &str, arguments: &serde_json::Value) {
        let pretty = serde_json::to_string_pretty(arguments).unwrap_or_default();
        if let Some(cm) = self.messages.write().iter_mut().find(|m| m.is_streaming) {
            if let Some(entry) = cm
                .tool_call_entries
                .iter_mut()
                .rfind(|e| e.name == name && e.phase == ToolCallPhase::Pending)
            {
                entry.phase = ToolCallPhase::Running;
                entry.arguments = pretty;
            } else {
                cm.tool_call_entries.push(ToolCallEntry {
                    name: name.to_string(),
                    phase: ToolCallPhase::Running,
                    arguments: pretty,
                    result: None,
                    is_error: false,
                });
            }
        }
    }

    pub fn tool_call_executed(
        &mut self,
        _id: &str,
        name: &str,
        is_error: bool,
        content: &serde_json::Value,
    ) {
        let mut found = false;
        for cm in self.messages.write().iter_mut() {
            if let Some(entry) = cm.tool_call_entries.iter_mut().find(|e| {
                e.name == name && matches!(e.phase, ToolCallPhase::Running | ToolCallPhase::Pending)
            }) {
                entry.phase = if is_error { ToolCallPhase::Error } else { ToolCallPhase::Completed };
                entry.is_error = is_error;
                entry.result = Some(content.clone());
                found = true;
                break;
            }
        }
        if !found {
            let phase = if is_error { ToolCallPhase::Error } else { ToolCallPhase::Completed };
            let target_idx = self.messages.read().iter().rposition(|m| {
                m.is_streaming || matches!(m.message.role, MessageRole::Assistant)
            });
            if let Some(idx) = target_idx {
                self.messages.write()[idx].tool_call_entries.push(ToolCallEntry {
                    name: name.to_string(),
                    phase,
                    arguments: String::new(),
                    result: Some(content.clone()),
                    is_error,
                });
            }
        }
    }
}

// ── 业务流程（发送 / 订阅 / 用户操作）────────────────────────────────────

impl ChatSignals {
    /// 发送消息：push user turn → send_text 入队。
    pub fn send_message(&mut self, chat_service_signal: ChatServiceSignal, text: String) {
        self.clear_pending();

        let seq_start = self.messages.read().iter().map(|m| m.sequence_order).max().unwrap_or(0) + 1;
        let mut seq = seq_start;
        self.push_user_turn(text.clone(), &mut seq);

        // ── 持久化 user 消息 ──
        if let Some(cm) = self.messages.read().last() {
            let storage = self.ctx.read().storage.clone();
            let cm = cm.clone();
            let pid = self.ctx.read().plan_id.clone();
            tokio::spawn(async move {
                storage.persist_message(&pid, &cm).await;
            });
        }

        let chat_svc = (*chat_service_signal.read()).clone();
        let Some(chat_svc) = chat_svc else {
            self.stop_streaming();
            self.append_to_last_assistant("*AI/Tools 服务未就绪，无法发起聊天。*");
            return;
        };

        if let Err(e) = chat_svc.send_text(text) {
            self.stop_streaming();
            self.append_to_last_assistant(&format!("*发送失败: {}*", e));
        }
    }

    /// 注册一次事件订阅（guard 存入 subscription signal，drop 时自动退订）。
    pub fn ensure_subscription(&mut self, chat_svc: &Arc<ChatService<FilePromptManager>>) {
        if self.subscription.read().is_none() {
            let chat_copy = *self;
            let guard = chat_svc.on_chat_with_guard(move |ev| handle_event(chat_copy, ev));
            self.subscription.set(Some(guard));
        }
    }

    /// 用户操作 `request_user_action` / 子 agent 挂起卡片后的回调。
    pub fn handle_user_action(
        &mut self,
        action: UIAction,
        choice: String,
        pending: PendingUI,
        chat_service_signal: ChatServiceSignal,
    ) {
        if let Some(asst_idx) = self.last_assistant_idx() {
            self.append_text(asst_idx, &format!("\n\n---\n\n**{choice}**\n\n"));
        }

        let seq = self.messages.read().iter().map(|m| m.sequence_order).max().unwrap_or(0) + 1;
        let mut seq = seq;
        self.push_assistant_placeholder(&mut seq);
        self.clear_pending();
        self.pending_tool_call_id.set(None);

        let chat_svc = (*chat_service_signal.read()).clone();
        let Some(chat_svc) = chat_svc else {
            self.append_to_last_assistant("\n\n*AI 服务未就绪，无法继续对话。*");
            self.stop_streaming();
            return;
        };

        if let Some(run_id) = pending.run_id.clone() {
            let input = serde_json::json!({ "choice": choice, "action_id": action.id });
            if let Err(e) = chat_svc.resume_sub_agent(&run_id, input) {
                self.append_to_last_assistant(&format!("\n\n*子 agent 恢复出错: {}*", e));
                self.stop_streaming();
            }
            return;
        }

        if let Err(e) = chat_svc.confirm_user_action(&pending.tool_call_id, &choice, &action.id) {
            self.append_to_last_assistant(&format!("\n\n*交互提交失败: {}*", e));
            self.stop_streaming();
        }
    }
}

// ── 事件消费（自由函数，因 on_chat_with_guard 要求 Fn）────────────────────

/// 消费 `ChatEvent`：流式写入、交互卡片、子 agent UI、Done/Error 收尾。
///
/// 自由函数（非方法），因为 `on_chat_with_guard` 要求 `Fn` 闭包，
/// 无法使用 `&mut self`。`ChatSignals` 是 `Copy`，闭包 move 一份副本，
/// Signal 句柄共享底层数据，修改对所有副本可见。
fn handle_event(mut chat: ChatSignals, ev: ServiceChatEvent) {
    match ev {
        ServiceChatEvent::Chat(ChatEvent::TextDelta(chunk)) => {
            tracing::debug!(target: "chat_flow", streaming = chat.is_streaming(), chunk_len = chunk.len(), chunk = %chunk, "TextDelta");
            chat.append_streaming(&chunk);
        }
        ServiceChatEvent::Chat(ChatEvent::ReasoningDelta(chunk)) => {
            tracing::debug!(target: "chat_flow", streaming = chat.is_streaming(), chunk_len = chunk.len(), chunk = %chunk, "ReasoningDelta");
            chat.append_streaming_reasoning(&chunk);
        }
        ServiceChatEvent::Chat(ChatEvent::RoundStart { .. }) => {
            tracing::debug!(target: "chat_flow", streaming = chat.is_streaming(), "RoundStart");
            if !chat.is_streaming() {
                let seq = chat.messages.read().iter().map(|m| m.sequence_order).max().unwrap_or(0) + 1;
                let mut seq = seq;
                chat.push_assistant_placeholder(&mut seq);
            }
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name }) if name == "request_user_action" => {
            tracing::debug!(target: "chat_flow", id = %id, name = %name, "ToolCallStart (request_user_action)");
            chat.pending_tool_call_id.set(Some(id));
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name }) => {
            tracing::debug!(target: "chat_flow", id = %id, name = %name, "ToolCallStart");
            chat.tool_call_start(&id, &name);
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallArgsDelta { id, delta }) => {
            tracing::debug!(target: "chat_flow", id = %id, delta_len = delta.len(), delta = %delta, "ToolCallArgsDelta");
            chat.tool_call_append_args(&id, &delta);
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallComplete { id, name, arguments }) => {
            tracing::debug!(target: "chat_flow", id = %id, name = %name, arguments = %arguments, streaming = chat.is_streaming(), "ToolCallComplete");
            if name != "request_user_action" {
                chat.tool_call_complete(&id, &name, &arguments);
            }
        }
        ServiceChatEvent::Chat(ChatEvent::ToolExecuted { id, name, is_error, content }) => {
            tracing::debug!(target: "chat_flow", id = %id, name = %name, is_error = is_error, content = %content, "ToolExecuted");
            chat.tool_call_executed(&id, &name, is_error, &content);
            if let Some(cm) = chat.current_streaming() {
                let storage = chat.ctx.read().storage.clone();
                let pid = chat.ctx.read().plan_id.clone();
                tokio::spawn(async move { storage.persist_message(&pid, &cm).await; });
            }
        }
        ServiceChatEvent::Chat(ChatEvent::RoundEnd { .. }) => {
            tracing::debug!(target: "chat_flow", streaming = chat.is_streaming(), "RoundEnd");
            if let Some(cm) = chat.current_streaming() {
                let storage = chat.ctx.read().storage.clone();
                let pid = chat.ctx.read().plan_id.clone();
                tokio::spawn(async move { storage.persist_message(&pid, &cm).await; });
            }
            chat.stop_streaming();
        }
        ServiceChatEvent::Chat(ChatEvent::UIActionRequest { message, actions, session_id }) => {
            let tool_call_id = chat.pending_tool_call_id.read().clone().unwrap_or_default();
            tracing::debug!(target: "chat_flow", tool_call_id = %tool_call_id, session_id = ?session_id, message = %message, actions_count = actions.len(), "UIActionRequest");
            chat.set_pending(PendingUI { message, actions, tool_call_id, run_id: session_id });
        }
        ServiceChatEvent::Done { cancelled } => {
            tracing::debug!(target: "chat_flow", cancelled = cancelled, streaming = chat.is_streaming(), has_pending = chat.has_pending(), "Done");
            chat.stop_streaming();
            chat.clear_pending();
            chat.pending_tool_call_id.set(None);
        }
        ServiceChatEvent::Error(e) => {
            tracing::error!(target: "chat_flow", error = %e, streaming = chat.is_streaming(), has_pending = chat.has_pending(), "聊天事件错误");
            if chat.pending_ui.read().is_none() {
                chat.stop_streaming();
                chat.append_to_last_assistant(&format!("\n\n*聊天出错: {}*", e));
            }
        }
    }
}
