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

use std::collections::HashMap;
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
    /// 关联的消息索引（在 ToolCallComplete 时从 streaming_idx 获取）
    pub msg_idx: Option<usize>,
}

/// 手动实现 PartialEq：跳过 Value 字段（不支持 PartialEq），比较其余字段。
impl PartialEq for ToolCallEntry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.phase == other.phase
            && self.arguments == other.arguments
            && self.is_error == other.is_error
            && self.msg_idx == other.msg_idx
    }
}

/// 消息状态（纯内存，Signal 均为 `Copy` 可直接进闭包/异步块）。
#[derive(Clone, Copy, PartialEq)]
pub struct ChatSignals {
    pub messages: Signal<Vec<Message>, SyncStorage>,
    pub reasoning_texts: Signal<Vec<Option<String>>, SyncStorage>,
    pub streaming_idx: Signal<Option<usize>, SyncStorage>,
    pub pending_ui: Signal<Option<PendingUI>, SyncStorage>,
    pub input_text: Signal<String, SyncStorage>,
    /// 最近一次 `request_user_action` tool call 的 id
    pub pending_tool_call_id: Signal<Option<String>, SyncStorage>,
    /// 当前 service 上已注册的事件订阅守卫（RAII 自动退订）
    pub subscription: Signal<Option<SubscriptionGuard>, SyncStorage>,
    /// 所有 tool 调用条目（key = tool_call_id）
    pub tool_call_entries: Signal<HashMap<String, ToolCallEntry>, SyncStorage>,
}

impl ChatSignals {
    /// 当前 streaming 消息索引。
    pub fn sidx(&self) -> Option<usize> {
        *self.streaming_idx.read()
    }

    /// 最后一条 Assistant 消息的索引。
    pub fn last_assistant_idx(&self) -> Option<usize> {
        self.messages
            .read()
            .iter()
            .rposition(|m| matches!(m.role, MessageRole::Assistant))
    }

    /// 推入用户消息 + Assistant 占位（作为第一轮 LLM 响应的载体）。
    pub fn push_user_turn(&mut self, user_text: String) -> usize {
        let user_msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text: user_text }),
            ..Default::default()
        };
        {
            let mut msgs = self.messages.write();
            msgs.push(user_msg);
            self.reasoning_texts.write().push(None);
        }
        let asst_idx = self.push_assistant_placeholder();
        self.streaming_idx.set(Some(asst_idx));
        asst_idx
    }

    /// 新建一条空 assistant 消息（一轮 LLM 响应的载体），返回其索引。
    pub fn push_assistant_placeholder(&mut self) -> usize {
        let asst_msg = Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text {
                text: String::new(),
            }),
            ..Default::default()
        };
        let idx;
        {
            let mut msgs = self.messages.write();
            idx = msgs.len();
            msgs.push(asst_msg);
            self.reasoning_texts.write().push(Some(String::new()));
        }
        idx
    }

    /// 向指定索引的消息追加文本。
    pub fn append_text(&mut self, idx: usize, chunk: &str) {
        if let Some(msg) = self.messages.write().get_mut(idx) {
            if let Some(MessageContent::Text { text }) = &mut msg.content {
                text.push_str(chunk);
            }
        }
    }

    /// 向当前 streaming 消息追加文本。
    pub fn append_streaming(&mut self, chunk: &str) {
        if let Some(idx) = self.sidx() {
            self.append_text(idx, chunk);
        }
    }

    /// 向指定索引追加推理文本。
    pub fn append_reasoning(&mut self, idx: usize, chunk: &str) {
        if let Some(Some(buf)) = self.reasoning_texts.write().get_mut(idx) {
            buf.push_str(chunk);
        }
    }

    /// 向当前 streaming 消息追加推理文本。
    pub fn append_streaming_reasoning(&mut self, chunk: &str) {
        if let Some(idx) = self.sidx() {
            self.append_reasoning(idx, chunk);
        }
    }

    /// 停止 streaming（保留当前内容）。
    pub fn stop_streaming(&mut self) {
        self.streaming_idx.set(None);
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
        self.reasoning_texts.set(vec![]);
        self.streaming_idx.set(None);
        self.pending_ui.set(None);
        self.pending_tool_call_id.set(None);
        self.tool_call_entries.set(HashMap::new());
    }

    // ── Tool 调用管理 ──

    /// 收到 ToolCallStart：创建新的 Pending 条目（跳过 request_user_action）。
    pub fn tool_call_start(&mut self, id: &str, name: &str) {
        self.tool_call_entries.write().insert(
            id.to_string(),
            ToolCallEntry {
                name: name.to_string(),
                phase: ToolCallPhase::Pending,
                arguments: String::new(),
                result: None,
                is_error: false,
                msg_idx: None,
            },
        );
    }

    /// 收到 ToolCallArgsDelta：追加参数片段。
    pub fn tool_call_append_args(&mut self, id: &str, delta: &str) {
        if let Some(entry) = self.tool_call_entries.write().get_mut(id) {
            entry.arguments.push_str(delta);
        }
    }

    /// 收到 ToolCallComplete：参数就绪，切换为 Running，并关联消息索引。
    pub fn tool_call_complete(&mut self, id: &str, name: &str, arguments: &serde_json::Value) {
        let mut entries = self.tool_call_entries.write();
        if let Some(entry) = entries.get_mut(id) {
            entry.phase = ToolCallPhase::Running;
            entry.arguments = serde_json::to_string_pretty(arguments).unwrap_or_default();
        } else {
            // 如果没收到 Start（比如直接从 Complete 开始），补建条目
            entries.insert(
                id.to_string(),
                ToolCallEntry {
                    name: name.to_string(),
                    phase: ToolCallPhase::Running,
                    arguments: serde_json::to_string_pretty(arguments).unwrap_or_default(),
                    result: None,
                    is_error: false,
                    msg_idx: None,
                },
            );
        }
    }

    /// 收到 ToolExecuted：执行结束，切换为 Completed / Error。
    pub fn tool_call_executed(
        &mut self,
        id: &str,
        name: &str,
        is_error: bool,
        content: &serde_json::Value,
    ) {
        let mut entries = self.tool_call_entries.write();
        let phase = if is_error {
            ToolCallPhase::Error
        } else {
            ToolCallPhase::Completed
        };
        if let Some(entry) = entries.get_mut(id) {
            entry.phase = phase;
            entry.is_error = is_error;
            entry.result = Some(content.clone());
        } else {
            entries.insert(
                id.to_string(),
                ToolCallEntry {
                    name: name.to_string(),
                    phase,
                    arguments: String::new(),
                    result: Some(content.clone()),
                    is_error,
                    msg_idx: None,
                },
            );
        }
    }

    /// 获取指定消息索引关联的所有 Tool 调用条目（按插入顺序）。
    pub fn tool_calls_for_message(&self, msg_idx: usize) -> Vec<(String, ToolCallEntry)> {
        let entries = self.tool_call_entries.read();
        entries
            .iter()
            .filter(|(_, entry)| entry.msg_idx == Some(msg_idx))
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect()
    }

    /// 在 ToolCallComplete 时将当前 streaming 消息关联到 tool call。
    pub fn associate_tool_call_to_message(&mut self, id: &str) {
        let msg_idx = *self.streaming_idx.read();
        if let Some(idx) = msg_idx {
            if let Some(entry) = self.tool_call_entries.write().get_mut(id) {
                entry.msg_idx = Some(idx);
            }
        }
    }
}

// ── 发送与事件消费 ────────────────────────────────────────────────────────

/// 发送消息：push user turn → send_text 入队。
pub fn send_message(chat_signal: ChatServiceSignal, mut chat: ChatSignals, text: String) {
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
            chat.append_streaming(&chunk);
        }
        ServiceChatEvent::Chat(ChatEvent::ReasoningDelta(chunk)) => {
            chat.append_streaming_reasoning(&chunk);
        }

        // ── 新一轮 LLM 响应开始：无 streaming 时新建 assistant 消息 ──
        ServiceChatEvent::Chat(ChatEvent::RoundStart { .. }) => {
            if chat.streaming_idx.read().is_none() {
                let idx = chat.push_assistant_placeholder();
                chat.streaming_idx.set(Some(idx));
            }
        }

        // ── 记录 request_user_action 的 tool_call_id ──
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name })
            if name == "request_user_action" =>
        {
            chat.pending_tool_call_id.set(Some(id));
        }

        // ── 其他 tool 调用：创建 Pending 条目 ──
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name }) => {
            chat.tool_call_start(&id, &name);
        }

        // ── Tool 参数增量 ──
        ServiceChatEvent::Chat(ChatEvent::ToolCallArgsDelta { id, delta }) => {
            chat.tool_call_append_args(&id, &delta);
        }

        // ── Tool 参数就绪，关联消息索引 ──
        ServiceChatEvent::Chat(ChatEvent::ToolCallComplete { id, name, arguments }) => {
            // request_user_action 不渲染为 ToolView：跳过补建与消息关联
            if name != "request_user_action" {
                chat.tool_call_complete(&id, &name, &arguments);
                chat.associate_tool_call_to_message(&id);
            }
        }

        // ── Tool 执行完成 ──
        ServiceChatEvent::Chat(ChatEvent::ToolExecuted { id, name, is_error, content }) => {
            chat.tool_call_executed(&id, &name, is_error, &content);
        }

        // ── 一轮 assistant 消息已写入历史：结束本消息 streaming ──
        ServiceChatEvent::Chat(ChatEvent::RoundEnd { .. }) => {
            chat.stop_streaming();
        }

        // ── UI 交互请求（主 agent：run_id=None；子 agent：run_id=Some(run_id)）──
        ServiceChatEvent::Chat(ChatEvent::UIActionRequest {
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
        ServiceChatEvent::Done { cancelled: _ } => {
            chat.stop_streaming();
            chat.clear_pending();
            chat.pending_tool_call_id.set(None);
        }

        // ── 错误 ──
        ServiceChatEvent::Error(e) => {
            tracing::error!("聊天事件错误: {}", e);
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
    let new_idx = chat.push_assistant_placeholder();
    chat.streaming_idx.set(Some(new_idx));
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
