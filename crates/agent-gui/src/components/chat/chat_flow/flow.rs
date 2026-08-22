//! 灵活模式聊天消息流转逻辑（纯内存，不持久化）。
//!
//! 基于 v2 [`planned_agent::ChatService`] 实现，支持：
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
use planned_agent::chat::{SubscriptionGuard, ChatEvent as ServiceChatEvent};
use planned_agent::ChatService;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::events::{ChatEvent, UIAction};
use planned_agent_prompt_manager::FilePromptManager;

use crate::services::chat_service::ChatServiceSignal;

// ── 数据结构 ──────────────────────────────────────────────────────────────

/// 待处理的 UI 交互（`request_user_action` 卡片状态）。
#[derive(Clone, PartialEq)]
pub struct PendingUI {
    /// 展示给用户的引导文本
    pub message: String,
    /// 用户可选的动作列表
    pub actions: Vec<UIAction>,
    /// 对应的 LLM tool_call_id（`confirm_user_action` 按此回填 tool 消息）
    pub tool_call_id: String,
    /// 子 agent 的 run_id（`UIActionRequest` 中的 `session_id` 字段）：
    /// 非空 = 子 agent 挂起，走 `resume_sub_agent` 路径；空 = 主 agent 交互。
    pub run_id: Option<String>,
}

/// Tool 调用的执行阶段。
#[derive(Clone, Debug, PartialEq)]
pub enum ToolCallPhase {
    /// 参数流式构建中（收到 ToolCallStart，尚未 ToolCallComplete）
    Pending,
    /// 参数已就绪，正在执行中（收到 ToolCallComplete，尚未 ToolExecuted）
    Running,
    /// 执行完成（收到 ToolExecuted，is_error = false）
    Completed,
    /// 执行出错（收到 ToolExecuted，is_error = true）
    Error,
}

/// 单次 Tool 调用的 UI 状态条目。
#[derive(Clone, Debug)]
pub struct ToolCallEntry {
    /// tool 名称
    pub name: String,
    /// 执行阶段
    pub phase: ToolCallPhase,
    /// 累积的参数原文（JSON 字符串）
    pub arguments: String,
    /// 执行结果（ToolExecuted.content）
    pub result: Option<serde_json::Value>,
    /// 是否出错
    pub is_error: bool,
}

/// 手动实现 PartialEq：跳过 Value 字段（不支持 PartialEq），比较其余字段。
impl PartialEq for ToolCallEntry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.phase == other.phase
            && self.arguments == other.arguments
            && self.is_error == other.is_error
    }
}

/// GUI 层的消息包装，自包含所有 UI 状态。
///
/// 替代了原来的并行信号（`reasoning_texts`、`streaming_idx`、`tool_call_entries`），
/// 让每条消息携带自身的推理文本、streaming 状态和工具调用条目。
#[derive(Clone)]
pub struct ChatMessage {
    /// 底层消息数据（role、content、reasoning_content、tool_calls 等）
    pub message: Message,
    /// 显示序号（递增，用于前端稳定排序/动画 key）
    pub sequence_order: u64,
    /// 是否正在 streaming（替代 `streaming_idx` 游标）
    pub is_streaming: bool,
    /// 本条消息关联的 Tool 调用条目（替代全局 `tool_call_entries` flat map）
    pub tool_call_entries: Vec<ToolCallEntry>,
}

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
    /// 持久化回调：ToolExecuted / RoundEnd 时调用，传入当前 ChatMessage
    pub persist: Signal<Option<Arc<dyn Fn(&ChatMessage) + Send + Sync>>, SyncStorage>,
}

impl ChatSignals {
    /// 获取当前 streaming 消息及其持久化回调（供 handle_event 在运行时内 spawn）。
    fn streaming_to_persist(&self) -> Option<(Arc<dyn Fn(&ChatMessage) + Send + Sync>, ChatMessage)> {
        let f = self.persist.read().clone()?;
        let cm = self.messages.read().iter().rev().find(|m| m.is_streaming).cloned()?;
        Some((f, cm))
    }

    /// 是否有任何消息正在 streaming。
    pub fn is_streaming(&self) -> bool {
        self.messages.read().iter().any(|m| m.is_streaming)
    }

    /// 最后一条 Assistant 消息的索引。
    pub fn last_assistant_idx(&self) -> Option<usize> {
        self.messages
            .read()
            .iter()
            .rposition(|m| matches!(m.message.role, MessageRole::Assistant))
    }

    /// 推入用户消息 + Assistant 占位（作为第一轮 LLM 响应的载体）。
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

    /// 新建一条空 assistant streaming 消息（一轮 LLM 响应的载体）。
    pub fn push_assistant_placeholder(&mut self, seq: &mut u64) {
        let asst_msg = Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text {
                text: String::new(),
            }),
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

    /// 向指定索引的消息追加文本。
    pub fn append_text(&mut self, idx: usize, chunk: &str) {
        if let Some(cm) = self.messages.write().get_mut(idx) {
            if let Some(MessageContent::Text { text }) = &mut cm.message.content {
                text.push_str(chunk);
            }
        }
    }

    /// 向当前 streaming 消息追加文本。
    pub fn append_streaming(&mut self, chunk: &str) {
        if let Some(cm) = self
            .messages
            .write()
            .iter_mut()
            .find(|m| m.is_streaming)
        {
            if let Some(MessageContent::Text { text }) = &mut cm.message.content {
                text.push_str(chunk);
            }
        }
    }

    /// 向当前 streaming 消息追加推理文本。
    pub fn append_streaming_reasoning(&mut self, chunk: &str) {
        if let Some(cm) = self
            .messages
            .write()
            .iter_mut()
            .find(|m| m.is_streaming)
        {
            let buf = cm
                .message
                .reasoning_content
                .get_or_insert_with(String::new);
            buf.push_str(chunk);
        }
    }

    /// 停止 streaming（将所有 is_streaming=true 的消息标记为 false）。
    pub fn stop_streaming(&mut self) {
        for cm in self.messages.write().iter_mut() {
            if cm.is_streaming {
                cm.is_streaming = false;
            }
        }
    }

    /// 向最后一条 Assistant 消息追加文本。
    pub fn append_to_last_assistant(&mut self, text: &str) {
        if let Some(idx) = self.last_assistant_idx() {
            self.append_text(idx, text);
        }
    }

    /// 设置待处理 UI 交互。
    pub fn set_pending(&mut self, state: PendingUI) {
        *self.pending_ui.write() = Some(state);
    }

    /// 清除待处理 UI 交互。
    pub fn clear_pending(&mut self) {
        self.pending_ui.set(None);
    }

    /// 是否有正在等待用户确认的交互卡片。
    pub fn has_pending(&self) -> bool {
        self.pending_ui.read().is_some()
    }

    /// 清空全部消息与相关状态。
    pub fn clear(&mut self) {
        self.messages.set(vec![]);
        self.pending_ui.set(None);
        self.pending_tool_call_id.set(None);
    }

    // ── Tool 调用管理（per-message） ──

    /// 在当前 streaming 消息上创建新的 Tool 调用条目。
    pub fn tool_call_start(&mut self, _id: &str, name: &str) {
        if let Some(cm) = self
            .messages
            .write()
            .iter_mut()
            .find(|m| m.is_streaming)
        {
            cm.tool_call_entries.push(ToolCallEntry {
                name: name.to_string(),
                phase: ToolCallPhase::Pending,
                arguments: String::new(),
                result: None,
                is_error: false,
            });
        }
    }

    /// 在当前 streaming 消息上追加 Tool 参数片段。
    pub fn tool_call_append_args(&mut self, id: &str, delta: &str) {
        if let Some(cm) = self
            .messages
            .write()
            .iter_mut()
            .find(|m| m.is_streaming)
        {
            // 按 name 匹配最后一个 Pending/Running 条目
            if let Some(entry) = cm
                .tool_call_entries
                .iter_mut()
                .rfind(|e| e.name == id || e.arguments.is_empty())
            {
                entry.arguments.push_str(delta);
            }
        }
    }

    /// 在当前 streaming 消息上标记 Tool 参数就绪。
    pub fn tool_call_complete(&mut self, _id: &str, name: &str, arguments: &serde_json::Value) {
        let pretty = serde_json::to_string_pretty(arguments).unwrap_or_default();
        if let Some(cm) = self
            .messages
            .write()
            .iter_mut()
            .find(|m| m.is_streaming)
        {
            // 找到同名的 Pending 条目并更新
            if let Some(entry) = cm
                .tool_call_entries
                .iter_mut()
                .rfind(|e| e.name == name && e.phase == ToolCallPhase::Pending)
            {
                entry.phase = ToolCallPhase::Running;
                entry.arguments = pretty;
            } else {
                // 补建条目（未收到 Start）
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

    /// 在对应消息上标记 Tool 执行完成。
    pub fn tool_call_executed(
        &mut self,
        _id: &str,
        name: &str,
        is_error: bool,
        content: &serde_json::Value,
    ) {
        let mut found = false;
        for cm in self.messages.write().iter_mut() {
            if let Some(entry) = cm
                .tool_call_entries
                .iter_mut()
                .find(|e| e.name == name && matches!(e.phase, ToolCallPhase::Running | ToolCallPhase::Pending))
            {
                entry.phase = if is_error {
                    ToolCallPhase::Error
                } else {
                    ToolCallPhase::Completed
                };
                entry.is_error = is_error;
                entry.result = Some(content.clone());
                found = true;
                break;
            }
        }
        if !found {
            // 兜底：在最后一条 streaming 消息（或最后一条 assistant）上补建
            let phase = if is_error {
                ToolCallPhase::Error
            } else {
                ToolCallPhase::Completed
            };
            let target_idx = self
                .messages
                .read()
                .iter()
                .rposition(|m| m.is_streaming || matches!(m.message.role, MessageRole::Assistant));
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

// ── 发送与事件消费 ────────────────────────────────────────────────────────

/// 发送消息：push user turn → send_text 入队。
pub fn send_message(chat_signal: ChatServiceSignal, mut chat: ChatSignals, text: String) {
    chat.clear_pending();

    // 计算当前 sequence_order 起始值
    let seq_start = chat.messages.read().iter().map(|m| m.sequence_order).max().unwrap_or(0) + 1;
    let mut seq = seq_start;
    chat.push_user_turn(text.clone(), &mut seq);

    // ── 持久化 user 消息 ──
    if let Some(ref persist_fn) = *chat.persist.read() {
        if let Some(cm) = chat.messages.read().last() {
            persist_fn(cm);
        }
    }

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
pub fn ensure_subscription(
    chat_svc: &Arc<ChatService<FilePromptManager>>,
    mut chat: ChatSignals,
) {
    if chat.subscription.read().is_none() {
        let guard = chat_svc.on_chat_with_guard(move |ev| handle_event(chat, ev));
        chat.subscription.set(Some(guard));
    }
}

/// 消费 `ChatEvent`：流式写入、交互卡片、子 agent UI、Done/Error 收尾。
fn handle_event(mut chat: ChatSignals, ev: ServiceChatEvent) {
    match ev {
        // ── 流式文本/推理 ──
        ServiceChatEvent::Chat(ChatEvent::TextDelta(chunk)) => {
            tracing::debug!(
                target: "chat_flow",
                streaming = chat.is_streaming(),
                chunk_len = chunk.len(),
                chunk = %chunk,
                "TextDelta"
            );
            chat.append_streaming(&chunk);
        }
        ServiceChatEvent::Chat(ChatEvent::ReasoningDelta(chunk)) => {
            tracing::debug!(
                target: "chat_flow",
                streaming = chat.is_streaming(),
                chunk_len = chunk.len(),
                chunk = %chunk,
                "ReasoningDelta"
            );
            chat.append_streaming_reasoning(&chunk);
        }

        // ── 新一轮 LLM 响应开始：无 streaming 消息时新建 assistant 占位 ──
        ServiceChatEvent::Chat(ChatEvent::RoundStart { .. }) => {
            tracing::debug!(
                target: "chat_flow",
                streaming = chat.is_streaming(),
                "RoundStart"
            );
            if !chat.is_streaming() {
                // 计算 sequence_order
                let seq = chat.messages.read().iter().map(|m| m.sequence_order).max().unwrap_or(0) + 1;
                let mut seq = seq;
                chat.push_assistant_placeholder(&mut seq);
            }
        }

        // ── 记录 request_user_action 的 tool_call_id ──
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name })
            if name == "request_user_action" =>
        {
            tracing::debug!(
                target: "chat_flow",
                id = %id,
                name = %name,
                "ToolCallStart (request_user_action — 记录 pending_tool_call_id)"
            );
            chat.pending_tool_call_id.set(Some(id));
        }

        // ── 其他 tool 调用：在 streaming 消息上创建条目 ──
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name }) => {
            tracing::debug!(
                target: "chat_flow",
                id = %id,
                name = %name,
                "ToolCallStart"
            );
            chat.tool_call_start(&id, &name);
        }

        // ── Tool 参数增量 ──
        ServiceChatEvent::Chat(ChatEvent::ToolCallArgsDelta { id, delta }) => {
            tracing::debug!(
                target: "chat_flow",
                id = %id,
                delta_len = delta.len(),
                delta = %delta,
                "ToolCallArgsDelta"
            );
            chat.tool_call_append_args(&id, &delta);
        }

        // ── Tool 参数就绪 ──
        ServiceChatEvent::Chat(ChatEvent::ToolCallComplete { id, name, arguments }) => {
            tracing::debug!(
                target: "chat_flow",
                id = %id,
                name = %name,
                arguments = %arguments,
                streaming = chat.is_streaming(),
                "ToolCallComplete"
            );
            // request_user_action 不渲染为 ToolView：跳过
            if name != "request_user_action" {
                chat.tool_call_complete(&id, &name, &arguments);
            }
        }

        // ── Tool 执行完成 ──
        ServiceChatEvent::Chat(ChatEvent::ToolExecuted { id, name, is_error, content }) => {
            tracing::debug!(
                target: "chat_flow",
                id = %id,
                name = %name,
                is_error = is_error,
                content = %content,
                "ToolExecuted"
            );
            chat.tool_call_executed(&id, &name, is_error, &content);
            if let Some((f, cm)) = chat.streaming_to_persist() {
                f(&cm);
            }
        }

        // ── 一轮 assistant 消息已写入历史：结束本消息 streaming ──
        ServiceChatEvent::Chat(ChatEvent::RoundEnd { .. }) => {
            tracing::debug!(
                target: "chat_flow",
                streaming = chat.is_streaming(),
                "RoundEnd — 停止 streaming"
            );
            if let Some((f, cm)) = chat.streaming_to_persist() {
                f(&cm);
            }
            chat.stop_streaming();
        }

        // ── UI 交互请求（主 agent：run_id=None；子 agent：run_id=Some(run_id)）──
        ServiceChatEvent::Chat(ChatEvent::UIActionRequest {
            message,
            actions,
            session_id,
        }) => {
            let tool_call_id = chat.pending_tool_call_id.read().clone().unwrap_or_default();
            tracing::debug!(
                target: "chat_flow",
                tool_call_id = %tool_call_id,
                session_id = ?session_id,
                message = %message,
                actions_count = actions.len(),
                actions = ?actions.iter().map(|a| &a.label).collect::<Vec<_>>(),
                "UIActionRequest"
            );
            chat.set_pending(PendingUI {
                message,
                actions,
                tool_call_id,
                run_id: session_id,
            });
        }

        // ── 对话结束 ──
        ServiceChatEvent::Done { cancelled } => {
            tracing::debug!(
                target: "chat_flow",
                cancelled = cancelled,
                streaming = chat.is_streaming(),
                has_pending = chat.has_pending(),
                "Done — 对话结束"
            );
            chat.stop_streaming();
            chat.clear_pending();
            chat.pending_tool_call_id.set(None);
        }

        // ── 错误 ──
        ServiceChatEvent::Error(e) => {
            tracing::error!(
                target: "chat_flow",
                error = %e,
                streaming = chat.is_streaming(),
                has_pending = chat.has_pending(),
                "聊天事件错误"
            );
            if chat.pending_ui.read().is_none() {
                chat.stop_streaming();
                chat.append_to_last_assistant(&format!("\n\n*聊天出错: {}*", e));
            }
        }
    }
}

// ── 用户操作回调 ──────────────────────────────────────────────────────────

/// 用户操作 `request_user_action` / 子 agent 挂起卡片后的回调。
///
/// - 子 agent 交互（`run_id` 非空）→ `resume_sub_agent`
/// - 父 agent 交互 → `confirm_user_action`
pub fn handle_user_action(
    action: UIAction,
    choice: String,
    pending: PendingUI,
    mut chat: ChatSignals,
    chat_signal: ChatServiceSignal,
) {
    // 回显用户选择到上一条 assistant 消息末尾（视觉上仍属同一气泡）
    if let Some(asst_idx) = chat.last_assistant_idx() {
        chat.append_text(asst_idx, &format!("\n\n---\n\n**{choice}**\n\n"));
    }

    // 新建下一条 assistant 消息作为本轮 LLM 响应的载体
    let seq = chat.messages.read().iter().map(|m| m.sequence_order).max().unwrap_or(0) + 1;
    let mut seq = seq;
    chat.push_assistant_placeholder(&mut seq);
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
