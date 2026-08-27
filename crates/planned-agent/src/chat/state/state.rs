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

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use anyhow::Result;
use planned_agent_ai_manager::AiManager;
use planned_agent_core::ai::types::Message;
use planned_agent_core::ai::types::MessageContent;
use planned_agent_core::ai::types::MessageRole;
use planned_agent_core::prompt::PromptManager;
use planned_agent_tool_manager::ToolRegistry;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::warn;

use super::command::RunState;
use crate::chat::service::ChatConfig;
use crate::chat::service::{ChatEvent, SubscriptionId};
use crate::chat::storage::ChatHistoryStore;

/// 无效的 store id，用于 system prompt 等不持久化的消息。
const NO_STORE_ID: &str = "";

// ── 共享 State ──────────────────────────────────────────────────────────────

/// v2 聊天服务的共享状态容器（`Arc<State<PM>>`）。
///
/// 通过 `Arc` 在后台 driver task 与 `ChatService` 各方法之间共享。
/// 字段都是 `pub(super)`：仅 chat 子模块可见，crate 外无法触达。
pub(crate) struct State<PM: PromptManager + Send + Sync + 'static> {
    pub(crate) ai_client: Arc<dyn planned_agent_core::ai::AiClient>,
    pub(crate) tool_registry: Arc<ToolRegistry>,
    pub(crate) prompt_manager: Arc<PM>,
    pub(crate) config: Mutex<ChatConfig>,
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
    config: &ChatConfig,
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
/// 内部用 `Mutex<Vec<(String, Message)>>` 同步互斥，其中 `String` 是 store 分配的
/// 持久化 ID（SQLite UUID / 内存空字符串），用于后续 `store.update` 定位。
///
/// `store` 在每次写入/清理操作时同步持久化，保证崩溃后可从 DB 恢复。
pub struct History {
    inner: Mutex<Vec<(String, Message)>>,
    store: Arc<dyn ChatHistoryStore>,
}

impl History {
    /// 创建历史，从 store 恢复已有消息填入内存。
    pub fn new(store: Arc<dyn ChatHistoryStore>) -> Self {
        let loaded = store.load();
        let inner: Vec<(String, Message)> = loaded
            .into_iter()
            .map(|m| (NO_STORE_ID.to_string(), m))
            .collect();
        Self {
            inner: Mutex::new(inner),
            store,
        }
    }

    /// 当前历史的完整快照（用于构造 LLM 请求）。
    pub fn snapshot(&self) -> Vec<Message> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|(_, m)| m.clone())
            .collect()
    }

    /// 首条消息是否是 system（用于 system prompt 幂等判断）。
    pub fn first_is_system(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .first()
            .map(|(_, m)| matches!(m.role, MessageRole::System))
            .unwrap_or(false)
    }

    /// 在首部插入一条 system 消息（幂等由调用方保证）。
    ///
    /// 不持久化：system prompt 是配置产物，下次 send 时 `inject_system_prompt`
    /// 会从模板重新注入，不需要存入 store。
    pub fn push_front_system(&self, text: String) {
        self.inner.lock().unwrap().insert(
            0,
            (
                NO_STORE_ID.to_string(),
                Message {
                    role: MessageRole::System,
                    content: Some(MessageContent::Text { text }),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    ..Default::default()
                },
            ),
        );
    }

    /// 写入一条 user 消息，返回 store_id。
    pub fn push_user(&self, msg: Message) -> String {
        let store_id = self.store.append(&msg);
        self.inner.lock().unwrap().push((store_id.clone(), msg));
        store_id
    }

    /// 写入一条 assistant 消息，返回 store_id。
    pub fn push_assistant(&self, msg: Message) -> String {
        let store_id = self.store.append(&msg);
        self.inner.lock().unwrap().push((store_id.clone(), msg));
        store_id
    }

    /// 写入一条 tool 消息（`tool_call_id` 对应、content 序列化为 JSON 文本），返回 store id。
    pub fn push_tool(&self, tool_call_id: &str, content: &Value) -> String {
        let json = serde_json::to_string(content).unwrap_or_else(|_| content.to_string());
        let msg = Message {
            role: MessageRole::Tool,
            content: Some(MessageContent::ToolResult {
                tool_call_id: tool_call_id.to_string(),
                content: json,
            }),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_calls: None,
            name: None,
            reasoning_content: None,
            ..Default::default()
        };
        let store_id = self.store.append(&msg);
        self.inner.lock().unwrap().push((store_id.clone(), msg));
        store_id
    }

    /// 回滚到指定 store_id 之前（即移除该 store_id 及之后的消息）。
    pub fn rollback_to_store_id(&self, store_id: &str) {
        let Some(idx) = self.find_idx_by_store_id(store_id) else {
            return;
        };
        let system_count = self.leading_system_count();
        self.inner.lock().unwrap().truncate(idx);
        self.store.rollback_to(idx.saturating_sub(system_count));
    }

    /// 前导 system 消息数量（store 中不存在 system，用于索引偏移换算）。
    fn leading_system_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .take_while(|(_, m)| matches!(m.role, MessageRole::System))
            .count()
    }

    /// 清空历史（会话重置）。
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
        self.store.clear();
    }

    /// 找到最后一条包含 `request_user_action` 的 assistant 消息的 store_id。
    pub fn find_pending_ui_tool_call_id(&self) -> Option<String> {
        let history = self.inner.lock().unwrap();
        for (store_id, msg) in history.iter().rev() {
            if matches!(msg.role, MessageRole::Assistant) {
                if let Some(tcs) = &msg.tool_calls {
                    if tcs.iter().any(|tc| tc.function.name == "request_user_action") {
                        return Some(store_id.clone());
                    }
                }
            }
        }
        None
    }

    /// 找到最后一条包含 `request_user_action` 的 assistant 消息的 tool_call_id。
    pub fn find_pending_tool_call_id(&self) -> Option<String> {
        let history = self.inner.lock().unwrap();
        for (_, msg) in history.iter().rev() {
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

    /// 根据 store_id 找到 inner 索引。
    fn find_idx_by_store_id(&self, store_id: &str) -> Option<usize> {
        self.inner.lock().unwrap().iter().position(|(id, _)| id == store_id)
    }

    /// 为指定 tool_call_id 伪造一条取消/中断的 tool 消息，返回 store id。
    /// 若该 tool_call_id 已有对应 tool 消息（已闭合），则跳过不添加。
    pub fn push_cancelled_tool(&self, tool_call_id: &str, reason: &str) -> String {
        let mut history = self.inner.lock().unwrap();
        let already_closed = history.iter().any(|(_, m)| {
            matches!(m.role, MessageRole::Tool) && m.tool_call_id.as_deref() == Some(tool_call_id)
        });
        if already_closed {
            return NO_STORE_ID.to_string();
        }
        let content = serde_json::json!({
            "error": true,
            "cancelled": true,
            "message": reason
        });
        let json = serde_json::to_string(&content).unwrap_or_default();
        let msg = Message {
            role: MessageRole::Tool,
            content: Some(MessageContent::ToolResult {
                tool_call_id: tool_call_id.to_string(),
                content: json,
            }),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_calls: None,
            name: None,
            reasoning_content: None,
            ..Default::default()
        };
        let store_id = self.store.append(&msg);
        history.push((store_id.clone(), msg));
        store_id
    }

    /// 若最后一条是带 tool_calls 的 assistant，且并非第一条 assistant，
    /// 移除它（用于达到 `max_tool_rounds` 时的清理）。移除时同步 store。
    pub fn pop_last_assistant_tool_calls_if_not_first(&self) {
        let len_before;
        {
            let mut history = self.inner.lock().unwrap();
            len_before = history.len();
            if let Some((_, last)) = history.last() {
                if matches!(last.role, MessageRole::Assistant)
                    && last.tool_calls.is_some()
                    && history
                        .iter()
                        .filter(|(_, m)| matches!(m.role, MessageRole::Assistant))
                        .count()
                        > 1
                {
                    history.pop();
                }
            }
        }
        let len_after = self.inner.lock().unwrap().len();
        if len_after < len_before {
            let system_count = self.leading_system_count();
            self.store
                .rollback_to(len_after.saturating_sub(system_count));
        }
    }
}

// ── Subscribers ─────────────────────────────────────────────────────────────

/// 事件订阅者列表（外层类型）。
pub struct Subscribers {
    inner: Arc<SubscribersInner>,
}

pub struct SubscribersInner {
    pub(crate) subs: Mutex<
        Vec<(
            SubscriptionId,
            Arc<dyn Fn(ChatEvent) + Send + Sync + 'static>,
        )>,
    >,
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

    pub(crate) fn subscribe(
        &self,
        handler: impl Fn(ChatEvent) + Send + Sync + 'static,
    ) -> SubscriptionId {
        let id = SubscriptionId(self.inner.next_id.fetch_add(1, Ordering::SeqCst));
        self.inner
            .subs
            .lock()
            .unwrap()
            .push((id, Arc::new(handler)));
        id
    }

    pub(crate) fn unsubscribe(&self, id: SubscriptionId) {
        self.inner
            .subs
            .lock()
            .unwrap()
            .retain(|(sid, _)| *sid != id);
    }

    pub fn emit(&self, ev: ChatEvent) {
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
                warn!("chat: 事件 handler 发生 panic，已隔离（不影响 driver 与其他订阅者）");
            }
        }
    }

    pub(crate) fn inner_weak(&self) -> Weak<SubscribersInner> {
        Arc::downgrade(&self.inner)
    }
}

impl Default for Subscribers {
    fn default() -> Self {
        Self::new()
    }
}
