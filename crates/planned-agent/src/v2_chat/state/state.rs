//! v2 聊天服务的共享状态。
//!
//! 该模块按"职责"把原来散落在 `service.rs` 的若干锁字段拆成三个具体类型：
//!
//! - [`History`]：内部消息历史（system / user / assistant / tool 全量）。
//!   所有读写都封装在自身，外部不再直接接触 `Mutex<Vec<Message>>`。
//! - [`Subscribers`]：事件订阅者列表 + 派发（含 panic 隔离）。
//! - [`RunState`] / `cancelled`：轻量原子/互斥状态。
//!
//! [`State`] 是把它们组装起来并通过 `Arc` 共享的容器；字段都是
//! `pub(super)`，确保只有同包可见，不暴露给 crate 外。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak, Mutex};

use anyhow::Result;
use planned_agent_ai_manager::AiManager;
use planned_agent_core::ai::types::Message;
use planned_agent_core::prompt::PromptManager;
use planned_agent_tool_manager::ToolRegistry;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::warn;

use super::command::RunState;
use crate::v2_chat::service::V2ChatConfig;
use crate::v2_chat::service::{SubscriptionId, V2ChatEvent};

// ── 共享 State ──────────────────────────────────────────────────────────────

/// v2 聊天服务的共享状态容器（`Arc<State<PM>>`）。
///
/// 通过 `Arc` 在后台 driver task 与 `V2ChatService` 各方法之间共享。
/// 字段都是 `pub(super)`：仅 v2_chat 子模块可见，crate 外无法触达。
pub(crate) struct State<PM: PromptManager + Send + Sync + 'static> {
    pub(crate) ai_client: Arc<dyn planned_agent_core::ai::AiClient>,
    pub(crate) tool_registry: Arc<ToolRegistry>,
    pub(crate) prompt_manager: Arc<PM>,
    pub(crate) config: Mutex<V2ChatConfig>,
    pub(crate) history: History,
    pub(crate) subscribers: Subscribers,
    pub(crate) cmd_tx: mpsc::UnboundedSender<super::command::Command>,
    pub(crate) driver_rx: Mutex<Option<mpsc::UnboundedReceiver<super::command::Command>>>,
    pub(crate) driver_started: AtomicBool,
    pub(crate) run_state: Mutex<RunState>,
    pub(crate) cancelled: Arc<AtomicBool>,
}

/// 通过 `AiManager` + 配置构造 `Arc<dyn AiClient>`（便于 `State::new` 复用）。
pub(crate) fn resolve_ai_client(
    ai_manager: &AiManager,
    config: &V2ChatConfig,
) -> Result<Arc<dyn planned_agent_core::ai::AiClient>> {
    let client = match &config.provider {
        Some(name) => ai_manager.get(name)?,
        None => ai_manager.default()?,
    };
    Ok(client)
}

// ── History ─────────────────────────────────────────────────────────────────

/// 内部消息历史（system / user / assistant / tool 全量，按序）。
///
/// 内部用 `Mutex<Vec<Message>>` 同步互斥（service 的同步锁足够；不跨
/// `.await` 持锁是 `History` 的不变式）。
pub struct History {
    inner: Mutex<Vec<Message>>,
}

impl History {
    /// 创建空历史。
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
        }
    }

    /// 当前历史的完整快照（用于构造 LLM 请求）。
    pub fn snapshot(&self) -> Vec<Message> {
        self.inner.lock().unwrap().clone()
    }

    /// 首条消息是否是 system（用于 system prompt 幂等判断）。
    pub fn first_is_system(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .first()
            .map(|m| matches!(m.role, planned_agent_core::ai::types::MessageRole::System))
            .unwrap_or(false)
    }

    /// 在首部插入一条 system 消息（幂等由调用方保证）。
    pub fn push_front_system(&self, text: String) {
        self.inner.lock().unwrap().insert(
            0,
            Message {
                role: planned_agent_core::ai::types::MessageRole::System,
                content: Some(planned_agent_core::ai::types::MessageContent::Text { text }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                ..Default::default()
            },
        );
    }

    /// 写入一条 user 消息，返回写入前的长度作为回滚点。
    pub fn push_user(&self, msg: planned_agent_core::ai::types::Message) -> usize {
        let mut guard = self.inner.lock().unwrap();
        let mark = guard.len();
        guard.push(msg);
        mark
    }

    /// 写入一条 assistant 消息。
    pub fn push_assistant(&self, msg: planned_agent_core::ai::types::Message) {
        self.inner.lock().unwrap().push(msg);
    }

    /// 写入一条 tool 消息（`tool_call_id` 对应、content 序列化为 JSON 文本）。
    pub fn push_tool(&self, tool_call_id: &str, content: &Value) {
        let json = serde_json::to_string(content).unwrap_or_else(|_| content.to_string());
        self.inner.lock().unwrap().push(Message {
            role: planned_agent_core::ai::types::MessageRole::Tool,
            content: Some(planned_agent_core::ai::types::MessageContent::ToolResult {
                tool_call_id: tool_call_id.to_string(),
                content: json,
            }),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_calls: None,
            name: None,
            reasoning_content: None,
            ..Default::default()
        });
    }

    /// 回滚历史到指定长度（用于对话失败时丢弃脏上下文）。
    pub fn rollback_to(&self, mark: usize) {
        self.inner.lock().unwrap().truncate(mark);
    }

    /// 清空历史（会话重置）。
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }

    /// 找到最后一条 assistant 消息里 `request_user_action` 的 tool_call_id。
    ///
    /// 用于子 agent resume：挂起时 `assistant(tool_calls=[request_user_action])`
    /// 保留在历史尾部，resume 时据此 tool_call_id 压入 tool 消息闭合协议。
    pub fn find_pending_ui_tool_call_id(&self) -> Option<String> {
        use planned_agent_core::ai::types::MessageRole;
        let history = self.inner.lock().unwrap();
        for msg in history.iter().rev() {
            if matches!(msg.role, MessageRole::Assistant) {
                if let Some(tcs) = &msg.tool_calls {
                    for tc in tcs {
                        if tc.function.name == "request_user_action" {
                            return Some(tc.id.clone());
                        }
                    }
                }
            }
        }
        None
    }

    /// 移除最后一条 assistant(tool_calls) 中没有对应 tool 消息跟随的调用；
    /// 若整条 assistant 一个都没闭合则整条移除。
    ///
    /// 用于「等待 UI 确认期间用户取消」的场景。
    pub fn clean_unclosed_assistant_tool_calls(&self) {
        use planned_agent_core::ai::types::{MessageRole, ToolCall};
        let mut history = self.inner.lock().unwrap();
        let Some(idx) = history.iter().rposition(|m| {
            matches!(m.role, MessageRole::Assistant)
                && m.tool_calls.as_ref().is_some_and(|tcs| !tcs.is_empty())
        }) else {
            return;
        };
        // 该 assistant 之后已闭合（有 tool 消息跟随）的调用 id
        let closed: HashSet<String> = history
            .iter()
            .skip(idx + 1)
            .filter_map(|m| m.tool_call_id.clone())
            .collect();
        let tcs = history[idx].tool_calls.take().unwrap_or_default();
        let closed_calls: Vec<ToolCall> = tcs
            .into_iter()
            .filter(|tc| closed.contains(&tc.id))
            .collect();
        if closed_calls.is_empty() {
            // 一个都没闭合：整条 assistant 移除
            history.remove(idx);
        } else {
            // 部分闭合：只保留已闭合的调用
            history[idx].tool_calls = Some(closed_calls);
        }
    }

    /// 若最后一条是带 tool_calls 的 assistant，且并非第一条 assistant，
    /// 移除它（用于达到 `max_tool_rounds` 时的清理）。
    pub fn pop_last_assistant_tool_calls_if_not_first(&self) {
        use planned_agent_core::ai::types::MessageRole;
        let mut history = self.inner.lock().unwrap();
        if let Some(last) = history.last() {
            if matches!(last.role, MessageRole::Assistant)
                && last.tool_calls.is_some()
                && history
                    .iter()
                    .filter(|m| matches!(m.role, MessageRole::Assistant))
                    .count()
                    > 1
            {
                history.pop();
            }
        }
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

// ── Subscribers ─────────────────────────────────────────────────────────────

/// 事件订阅者列表（外层类型）。
///
/// 内部用 `Arc<Inner>` 共享，使 [`crate::v2_chat::SubscriptionGuard`] 能在
/// Drop 时通过 `Weak<Inner>` 反查回本表并退订——guard 不延长本类型的寿命，
/// service 全部 drop 后 guard 的 Drop 自动成为 no-op。
///
/// `emit` 在锁外调用每个 handler，避免 handler 内反向调用 service 方法时死锁；
/// `catch_unwind` 隔离单个 handler 的 panic，不影响其他订阅者与 driver。
pub struct Subscribers {
    inner: Arc<SubscribersInner>,
}

/// `Subscribers` 共享内层：`Vec` + ID 分配器。`SubscriptionGuard::drop` 通过
/// `Weak<SubscribersInner>` 反查本表，若 `Weak::upgrade` 失败（service 全部 drop）
/// 则退订操作自动成为 no-op——guard 永远不会延长 service 的寿命。
///
/// **可见性说明**：标 `pub` 而非 `pub(crate)`，是为了让
/// `pub struct SubscriptionGuard { inner: Weak<SubscribersInner>, ... }` 字段合法
/// ——Rust 要求 `pub` struct 的所有字段类型至少与 struct 同可见，guard 是 `pub`
/// 故字段类型也必须是 `pub`。crate 外**不应**直接构造 `SubscribersInner`
/// （无公开构造函数）；`SubscriptionGuard::new` 是 `pub(crate)`，crate 外拿不到。
pub struct SubscribersInner {
    /// 订阅者列表 + 闭包。字段 `pub(crate)`：guard 的 `detach_inner` 需要直接
    /// 操作本字段。
    pub(crate) subs: Mutex<Vec<(SubscriptionId, Arc<dyn Fn(V2ChatEvent) + Send + Sync + 'static>)>>,
    next_id: AtomicU64,
}

impl Subscribers {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SubscribersInner {
                subs: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// 注册事件监听，返回订阅 ID。
    pub(crate) fn subscribe(
        &self,
        handler: impl Fn(V2ChatEvent) + Send + Sync + 'static,
    ) -> SubscriptionId {
        let id = SubscriptionId(self.inner.next_id.fetch_add(1, Ordering::SeqCst));
        self.inner.subs.lock().unwrap().push((id, Arc::new(handler)));
        id
    }

    /// 取消事件订阅（按 ID）。
    ///
    /// `SubscriptionGuard::drop` 通过 `Weak<SubscribersInner>` 间接调用本方法。
    pub(crate) fn unsubscribe(&self, id: SubscriptionId) {
        self.inner.subs.lock().unwrap().retain(|(sid, _)| *sid != id);
    }

    /// 派发事件到所有订阅者（锁外调用，panic 隔离）。
    pub fn emit(&self, ev: V2ChatEvent) {
        // 锁外调用 handler：避免 handler 内反向调用 service 方法时死锁
        let subs: Vec<_> = self
            .inner
            .subs
            .lock()
            .unwrap()
            .iter()
            .map(|(id, h)| (*id, h.clone()))
            .collect();
        for (_, handler) in subs {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(ev.clone())));
            if result.is_err() {
                warn!("v2_chat: 事件 handler 发生 panic，已隔离（不影响 driver 与其他订阅者）");
            }
        }
    }

    /// 取得内部 `Arc<SubscribersInner>` 的 `Weak` 句柄（供 [`SubscriptionGuard`] 使用）。
    ///
    /// `pub(crate)`：仅 crate 内可见（被 `service::service::on_chat_with_guard`
    /// 调用）。crate 外不能直接拿 `Weak<SubscribersInner>`，但可以拿到
    /// [`SubscriptionGuard`] 这个封装。
    pub(crate) fn inner_weak(&self) -> Weak<SubscribersInner> {
        Arc::downgrade(&self.inner)
    }
}

impl Default for Subscribers {
    fn default() -> Self {
        Self::new()
    }
}

// （state.rs 不再做 RunStateGuard：直接保留 Mutex<RunState> 字段即可，
// 调用方写 `*state.run_state.lock().unwrap() = RunState::X` 已经够直白；
// 该字段包内 5 处读写，无需再造一层封装。）