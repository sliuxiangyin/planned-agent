//! 灵活模式聊天消息流转逻辑（纯内存，不持久化）。
//!
//! 基于 v2 [`planned_agent::V2ChatService`] 实现，支持：
//! - 父 agent 的 `request_user_action` 交互（`UIActionRequest.run_id=None`）
//! - 子 agent 的 UI 交互（`UIActionRequest.run_id=Some(run_id)`，走
//!   `resume_sub_agent` 恢复）
//!
//! 事件驱动：`send_text` 入队后，事件经 `on_chat` 订阅实时写入 UI。
//!
//! 主/子 agent 的 UI 交互统一用 `ChatEvent::UIActionRequest` 表达，前端只按
//! `run_id`（即 `session_id`，子 agent 的 run_id）是否为空区分：空走
//! `confirm_user_action`，非空走 `resume_sub_agent`。

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::v2_chat::{SubscriptionGuard, V2ChatEvent};
use planned_agent::V2ChatService;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::events::{ChatEvent, UIAction};
use planned_agent_prompt_manager::FilePromptManager;

use crate::services::chat_service::V2ChatServiceSignal;

// ── 数据结构 ──────────────────────────────────────────────────────────────

/// 待处理的 UI 交互（`request_user_action` 卡片状态）。
#[derive(Clone)]
pub(crate) struct PendingUI {
    /// 展示给用户的引导文本
    pub(crate) message: String,
    /// 用户可选的动作列表
    pub(crate) actions: Vec<UIAction>,
    /// 对应的 LLM tool_call_id（`confirm_user_action` 按此回填 tool 消息）
    pub(crate) tool_call_id: String,
    /// 子 agent 的 run_id（`UIActionRequest` 中的 `session_id` 字段）：
    /// 非空 = 子 agent 挂起，走 `resume_sub_agent` 路径；空 = 主 agent 交互。
    pub(crate) run_id: Option<String>,
}

/// 消息状态（纯内存，Signal 均为 `Copy` 可直接进闭包/异步块）。
#[derive(Clone, Copy)]
pub(crate) struct ChatSignals {
    pub(crate) messages: Signal<Vec<Message>, SyncStorage>,
    pub(crate) reasoning_texts: Signal<Vec<Option<String>>, SyncStorage>,
    pub(crate) streaming_idx: Signal<Option<usize>, SyncStorage>,
    pub(crate) pending_ui: Signal<Option<PendingUI>, SyncStorage>,
    pub(crate) input_text: Signal<String, SyncStorage>,
    /// 最近一次 `request_user_action` tool call 的 id
    pub(crate) pending_tool_call_id: Signal<Option<String>, SyncStorage>,
    /// 当前 service 上已注册的事件订阅守卫（RAII 自动退订）
    pub(crate) subscription: Signal<Option<SubscriptionGuard>, SyncStorage>,
}

impl ChatSignals {
    /// 当前 streaming 消息索引。
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

    /// 推入用户消息 + Assistant 占位。
    pub(crate) fn push_user_turn(&mut self, user_text: String) -> usize {
        let user_msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text: user_text }),
            ..Default::default()
        };
        let asst_msg = Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text {
                text: String::new(),
            }),
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

    /// 向指定索引的消息追加文本。
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

    /// 是否有正在等待用户确认的交互卡片。
    pub(crate) fn has_pending(&self) -> bool {
        self.pending_ui.read().is_some()
    }

    /// 清空全部消息与相关状态。
    pub(crate) fn clear(&mut self) {
        self.messages.set(vec![]);
        self.reasoning_texts.set(vec![]);
        self.streaming_idx.set(None);
        self.pending_ui.set(None);
        self.pending_tool_call_id.set(None);
    }
}

// ── 发送与事件消费 ────────────────────────────────────────────────────────

/// 发送消息：push user turn → send_text 入队。
pub(crate) fn send_message(chat_signal: V2ChatServiceSignal, mut chat: ChatSignals, text: String) {
    chat.clear_pending();
    chat.push_user_turn(text.clone());

    let chat_svc = (*chat_signal.read()).clone();
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

/// 注册一次事件订阅（guard 存入 subscription signal，drop 时自动退订）。
/// 首次 service ready 后调用一次即可，重复调用无效（已订阅则跳过）。
pub(crate) fn ensure_subscription(
    chat_svc: &Arc<V2ChatService<FilePromptManager>>,
    mut chat: ChatSignals,
) {
    if chat.subscription.read().is_none() {
        let guard = chat_svc.on_chat_with_guard(move |ev| handle_event(chat, ev));
        chat.subscription.set(Some(guard));
    }
}

/// 消费 `V2ChatEvent`：流式写入、交互卡片、子 agent UI、Done/Error 收尾。
fn handle_event(mut chat: ChatSignals, ev: V2ChatEvent) {
    match ev {
        // ── 流式文本/推理 ──
        V2ChatEvent::Chat(ChatEvent::TextDelta(chunk)) => {
            chat.append_streaming(&chunk);
        }
        V2ChatEvent::Chat(ChatEvent::ReasoningDelta(chunk)) => {
            chat.append_streaming_reasoning(&chunk);
        }

        // ── 记录 request_user_action 的 tool_call_id ──
        V2ChatEvent::Chat(ChatEvent::ToolCallStart { id, name })
            if name == "request_user_action" =>
        {
            chat.pending_tool_call_id.set(Some(id));
        }

        // ── UI 交互请求（主 agent：run_id=None；子 agent：run_id=Some(run_id)）──
        V2ChatEvent::Chat(ChatEvent::UIActionRequest {
            message,
            actions,
            session_id,
        }) => {
            let tool_call_id = chat.pending_tool_call_id.read().clone().unwrap_or_default();
            chat.set_pending(PendingUI {
                message,
                actions,
                tool_call_id,
                run_id: session_id,
            });
        }

        // ── 对话结束 ──
        V2ChatEvent::Done { cancelled } => {
            chat.stop_streaming();
            chat.clear_pending();
            chat.pending_tool_call_id.set(None);
            // if cancelled {

            // chat.append_to_last_assistant("\n\n*已停止*");
            // }
        }

        // ── 错误 ──
        V2ChatEvent::Error(e) => {
            tracing::error!("灵活模式事件错误: {}", e);
            if chat.pending_ui.read().is_none() {
                chat.stop_streaming();
                chat.append_to_last_assistant(&format!("\n\n*聊天出错: {}*", e));
            }
        }

        _ => {}
    }
}

// ── 用户操作回调 ──────────────────────────────────────────────────────────

/// 用户操作 `request_user_action` / 子 agent 挂起卡片后的回调。
///
/// - 子 agent 交互（`run_id` 非空）→ `resume_sub_agent`
/// - 父 agent 交互 → `confirm_user_action`
pub(crate) fn handle_user_action(
    action: UIAction,
    choice: String,
    pending: PendingUI,
    mut chat: ChatSignals,
    chat_signal: V2ChatServiceSignal,
) {
    let Some(asst_idx) = chat.last_assistant_idx() else {
        return;
    };

    // 回显用户选择
    chat.append_to_last_assistant(&format!("\n\n---\n\n**{choice}**\n\n"));
    chat.streaming_idx.set(Some(asst_idx));
    chat.clear_pending();
    chat.pending_tool_call_id.set(None);

    let chat_svc = (*chat_signal.read()).clone();
    let Some(chat_svc) = chat_svc else {
        chat.append_to_last_assistant("\n\n*AI 服务未就绪，无法继续对话。*");
        chat.stop_streaming();
        return;
    };

    // ── 路径 A：子 agent 挂起 → 恢复会话（resume_sub_agent 同步发信号）──
    if let Some(run_id) = pending.run_id.clone() {
        let input = serde_json::json!({ "choice": choice, "action_id": action.id });
        chat.streaming_idx.set(Some(asst_idx));
        if let Err(e) = chat_svc.resume_sub_agent(&run_id, input) {
            chat.append_to_last_assistant(&format!("\n\n*子 agent 恢复出错: {}*", e));
            chat.stop_streaming();
        }
        return;
    }

    // ── 路径 B：父 agent 自身交互 → confirm_user_action ──
    if let Err(e) = chat_svc.confirm_user_action(&pending.tool_call_id, &choice, &action.id) {
        chat.append_to_last_assistant(&format!("\n\n*交互提交失败: {}*", e));
        chat.stop_streaming();
    }
}
