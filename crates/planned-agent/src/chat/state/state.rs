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
use tokio::sync::{mpsc, watch};
use tracing::warn;

use super::command::RunState;
use crate::chat::service::ChatConfig;
use crate::chat::service::{ChatEvent, SubscriptionId};
use crate::chat::storage::{ChatHistoryStore, ErrorType, StoreMessage};

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
    /// 本地取消 watch：任何取消来源（本服务 `stop()` 或上游父级取消）都会把它置为 `true`，
    /// 用于向下游子 agent 传播（其把本 watch 作为上游），并作为 driver 可等待的取消信号。
    pub(crate) cancel_tx: watch::Sender<bool>,
    /// 上游父级取消接收端。当本服务是某个父服务创建的子 agent 时持有（主 agent 为 `None`）。
    pub(crate) upstream_cancel: Mutex<Option<watch::Receiver<bool>>>,
}

impl<PM: PromptManager + Send + Sync + 'static> State<PM> {
    /// 统一取消入口：置原子标志 + 广播本地取消 watch（幂等）。
    pub(crate) fn mark_cancelled(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let _ = self.cancel_tx.send(true);
    }

    /// 本层（父）本地取消 watch 的接收端 —— 交给下游子 agent 作为其上游取消。
    pub(crate) fn cancel_rx(&self) -> watch::Receiver<bool> {
        self.cancel_tx.subscribe()
    }

    /// 判断是否已被取消（自身 `stop()` 或上游父级取消传导）。
    ///
    /// 供 driver 的各"轮询式取消检查点"（LLM 每 chunk、执行前 break 等）使用。
    /// 与 [`State::cancel_signal`]（可等待）互补：此处是非阻塞快照判断。
    pub(crate) fn is_cancelled_effective(&self) -> bool {
        if self.cancelled.load(Ordering::SeqCst) {
            return true;
        }
        self.upstream_cancel
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| *r.borrow())
            .unwrap_or(false)
    }

    /// 记录上游父级取消接收端（由 runner 在创建子 agent 后调用）。
    pub(crate) fn attach_upstream(&self, rx: watch::Receiver<bool>) {
        *self.upstream_cancel.lock().unwrap() = Some(rx);
    }

    /// 等待"本服务被取消"（含自身 `stop()` 或上游父级取消传导）。
    ///
    /// 供 driver 在阻塞等待子工具/子 agent 结果的 `.await` 处用 `select!` 竞速。
    /// 若先被上游取消触发，会先 `mark_cancelled()`（把取消传导给自己，使更下游子 agent 感知）。
    pub(crate) async fn cancel_signal(&self) {
        let mut local = self.cancel_tx.subscribe();
        // 已经处于已取消状态则立即返回
        if *local.borrow_and_update() {
            return;
        }
        let upstream = self.upstream_cancel.lock().unwrap().clone();
        match upstream {
            None => {
                let _ = local.wait_for(|v| *v).await;
            }
            Some(mut up) => {
                tokio::select! {
                    r = local.wait_for(|v| *v) => { let _ = r; }
                    r = up.wait_for(|v| *v) => {
                        let _ = r;
                        self.mark_cancelled();
                    }
                }
            }
        }
    }
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
/// 内部用 `Mutex<Vec<(String, StoreMessage)>>` 同步互斥，其中 `String` 是 store 分配的
/// 持久化 ID（SQLite UUID / 内存空字符串），`StoreMessage` 包含 `Message` + `ErrorType`。
///
/// `store` 在每次写入/清理操作时同步持久化，保证崩溃后可从 DB 恢复。
pub struct History {
    inner: Mutex<Vec<(String, StoreMessage)>>,
    store: Arc<dyn ChatHistoryStore>,
    tool_registry: Arc<ToolRegistry>,
}

impl History {
    /// 创建历史，从 store 恢复已有消息填入内存。
    pub async fn new(store: Arc<dyn ChatHistoryStore>, tool_registry: Arc<ToolRegistry>) -> Self {
        let loaded = store.load().await;
        let inner: Vec<(String, StoreMessage)> = loaded
            .into_iter()
            .map(|sm| (NO_STORE_ID.to_string(), sm))
            .collect();
        Self {
            inner: Mutex::new(inner),
            store,
            tool_registry,
        }
    }

    /// 当前历史的完整快照（用于构造 LLM 请求）。
    pub fn snapshot(&self) -> Vec<Message> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|(_, sm)| sm.message.clone())
            .collect()
    }

    /// 当前历史的完整快照（含错误类型元数据，用于 GUI 历史加载）。
    pub fn snapshot_store(&self) -> Vec<StoreMessage> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|(_, sm)| sm.clone())
            .collect()
    }

    /// 首条消息是否是 system（用于 system prompt 幂等判断）。
    pub fn first_is_system(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .first()
            .map(|(_, sm)| matches!(sm.message.role, MessageRole::System))
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
                StoreMessage::normal(Message {
                    role: MessageRole::System,
                    content: Some(MessageContent::Text { text }),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    ..Default::default()
                }),
            ),
        );
    }

    /// 写入一条 user 消息，返回 store_id。
    pub async fn push_user(&self, msg: Message) -> String {
        let sm = StoreMessage::normal(msg);
        let store_id = self.store.append(&sm).await;
        self.inner.lock().unwrap().push((store_id.clone(), sm));
        store_id
    }

    /// 写入一条 assistant 消息，返回 store_id。
    ///
    /// 自动检查 tool_calls 中是否包含 SubAgent 工具，设置 `is_agent_tool`。
    pub async fn push_assistant(&self, msg: Message) -> String {
        let is_agent_tool = msg.tool_calls.as_ref().map(|tcs| {
            tcs.iter().any(|tc| {
                self.tool_registry
                    .get_metadata(&tc.function.name)
                    .map(|m| matches!(m.source, planned_agent_core::tool_registry::types::ToolSource::SubAgent { .. }))
                    .unwrap_or(false)
            })
        }).unwrap_or(false);
        let mut sm = StoreMessage::normal(msg);
        sm.is_agent_tool = is_agent_tool;
        let store_id = self.store.append(&sm).await;
        self.inner.lock().unwrap().push((store_id.clone(), sm));
        store_id
    }

    /// 写入一条 tool 消息（`tool_call_id` 对应、content 序列化为 JSON 文本），返回 store id。
    ///
    /// `is_error_type` 默认 `ErrorType::None`；工具执行失败时由 `execute_backend_tool_call`
    /// 传入 `ErrorType::ExecutionError`，使历史回显能正确显示 Error 图标。
    pub async fn push_tool(
        &self,
        tool_call_id: &str,
        content: &Value,
        is_error_type: ErrorType,
    ) -> String {
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
        let sm = StoreMessage::new(msg, is_error_type);
        let store_id = self.store.append(&sm).await;
        self.inner.lock().unwrap().push((store_id.clone(), sm));
        store_id
    }

    /// 清空历史（会话重置）。
    pub async fn clear(&self) {
        self.inner.lock().unwrap().clear();
        self.store.clear().await;
    }

    /// 找到最后一条包含 `request_user_action` 的 assistant 消息的 store_id。
    pub fn find_pending_ui_tool_call_id(&self) -> Option<String> {
        let history = self.inner.lock().unwrap();
        for (store_id, sm) in history.iter().rev() {
            if matches!(sm.message.role, MessageRole::Assistant) {
                if let Some(tcs) = &sm.message.tool_calls {
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
        for (_, sm) in history.iter().rev() {
            if matches!(sm.message.role, MessageRole::Assistant) {
                if let Some(tcs) = &sm.message.tool_calls {
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

    /// 为指定 tool_call_id 伪造一条取消/中断的 tool 消息，返回 store id。
    /// 若该 tool_call_id 已有对应 tool 消息（已闭合），则跳过不添加。
    pub async fn push_cancelled_tool(&self, tool_call_id: &str, reason: &str) -> String {
        // 先检查是否已闭合（短暂持锁，不跨 await）
        {
            let history = self.inner.lock().unwrap();
            let already_closed = history.iter().any(|(_, sm)| {
                matches!(sm.message.role, MessageRole::Tool)
                    && sm.message.tool_call_id.as_deref() == Some(tool_call_id)
            });
            if already_closed {
                return NO_STORE_ID.to_string();
            }
        }
        let msg = Message {
            role: MessageRole::Tool,
            content: Some(MessageContent::ToolResult {
                tool_call_id: tool_call_id.to_string(),
                content: reason.to_string(),
            }),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_calls: None,
            name: None,
            reasoning_content: None,
            ..Default::default()
        };
        let sm = StoreMessage::new(msg, ErrorType::Cancelled);
        let store_id = self.store.append(&sm).await;
        self.inner.lock().unwrap().push((store_id.clone(), sm));
        store_id
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

// ── Cancelled Tool Content ────────────────────────────────────────────────

/// 工具被取消/中断时的 content 结构体。
///
/// 由 `close_tool_calls_with_reason`（emit）和 `History::push_cancelled_tool`（持久化）共用，
/// 保证实时事件与历史加载的格式一致。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolExecuteError {
    pub error: bool,
    pub cancelled: bool,
    pub message: String,
}

impl ToolExecuteError {
    /// 构造 ToolExecuteError。
    pub fn build(reason: &str) -> Self {
        Self {
            error: false,
            cancelled: false,
            message: reason.to_string(),
        }
    }

    /// 设置 error 标志（返回 self 以支持链式调用）。
    pub fn set_error(mut self, v: bool) -> Self {
        self.error = v;
        self
    }

    /// 设置 cancelled 标志（返回 self 以支持链式调用）。
    pub fn set_cancelled(mut self, v: bool) -> Self {
        self.cancelled = v;
        self
    }

    /// 转换为 JSON 字符串（用于 `Message.content` 持久化）。
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// 转换为 `serde_json::Value`（用于 `ToolExecuted.content` 事件 emit）。
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}
