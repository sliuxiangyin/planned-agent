//! 消息历史持久化接口。
//!
//! [`ChatHistoryStore`] trait 定义了与 [`History`]（`state/state.rs`）操作一一对应的
//! 消息级持久化接口。默认实现 [`InMemoryStore`] 不落盘（内存会话），宿主（GUI / 测试）
//! 可注入自定义实现（如 SQLite）。
//!
//! # 设计原则
//!
//! - **构造时绑定会话**：实现方在构造时绑定 session 标识（如 plan_id），
//!   trait 本身无 session 概念——`History` 是单会话上下文，与 store 一一对应。
//! - **同步签名**：`append` 是同步的，实现方可内部 `tokio::spawn` fire-and-forget
//!   （与调用方的生命周期一致，崩溃丢最后几条，可接受）。
//! - **snapshot 不经 store**：LLM 请求构造的 `history.snapshot()` 直接读内存，
//!   store 只负责写穿透持久化。
//! - **统一 `String` ID**：所有实现使用 `String` 作为持久化 ID 类型，
//!   `InMemoryStore` 返回进程内唯一自增序号，SQLite 实现直接返回 UUID。

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use planned_agent_core::ai::types::Message;

// ── ErrorType ─────────────────────────────────────────────────────────────

/// 消息错误类型（用于区分正常消息、执行错误、中断取消）。
///
/// 与数据库列 `is_error_type: INTEGER` 一一对应（0/1/2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(i32)]
pub enum ErrorType {
    /// 无错误
    None = 0,
    /// 工具执行失败
    ExecutionError = 1,
    /// 被中断/取消
    Cancelled = 2,
}

impl ErrorType {
    /// 从数据库 `INTEGER` 值转换。
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => ErrorType::ExecutionError,
            2 => ErrorType::Cancelled,
            _ => ErrorType::None,
        }
    }
}

// ── StoreMessage ──────────────────────────────────────────────────────────

/// 持久化消息包装：`Message` + 错误类型元数据。
///
/// `ChatHistoryStore` 的所有方法统一使用 `StoreMessage`，
/// 替代直接使用 `Message`，使 store 层可以携带错误分类信息。
#[derive(Debug, Clone)]
pub struct StoreMessage {
    pub message: Message,
    pub is_error_type: ErrorType,
    /// 该 assistant 消息的 tool_calls 中是否包含 SubAgent 工具。
    ///
    /// 由 `History::push_assistant` 根据 `ToolRegistry` metadata 自动设置。
    /// `build_bubbles` 读取此字段为对应 `ToolViewData` 设置 `is_sub_agent`。
    pub is_agent_tool: bool,
}

impl StoreMessage {
    pub fn new(message: Message, is_error_type: ErrorType) -> Self {
        Self {
            message,
            is_error_type,
            is_agent_tool: false,
        }
    }

    /// 无错误的便捷构造。
    pub fn normal(message: Message) -> Self {
        Self::new(message, ErrorType::None)
    }
}

// ── ChatHistoryStore trait ────────────────────────────────────────────────

/// 消息级持久化接口，与 `History` 的操作一一对应。
///
/// 使用 `String` 作为持久化 ID 类型：
/// - `InMemoryStore` 返回唯一自增序号（不落盘，但 id 仍须唯一）；
/// - SQLite 实现返回 UUID 主键。
#[async_trait]
pub trait ChatHistoryStore: Send + Sync {
    /// 恢复历史（`History::new` 时调用一次，填入内存热数据）。
    async fn load(&self) -> Vec<StoreMessage>;

    /// 追加一条消息，返回持久化 ID。
    async fn append(&self, msg: &StoreMessage) -> String;

    /// 根据 ID 更新消息内容。
    async fn update(&self, id: &str, msg: &StoreMessage);

    /// 清空会话（`clear` / `reset_session` 后调用）。
    async fn clear(&self);
}

// ── InMemoryStore ─────────────────────────────────────────────────────────

/// `InMemoryStore` 的进程内自增 id 计数器。
///
/// 内存实现不落盘，但 **store_id 必须唯一** —— `History` 里按 id 定位的路径
/// （如 `upsert_tool` 的就地更新）依赖它；否则会命中错误条目、覆盖别的消息。
static NEXT_MEMORY_STORE_ID: AtomicUsize = AtomicUsize::new(0);

/// 默认内存实现：不持久化消息内容，但 `append` 仍返回**唯一** id。
///
/// 用于子 agent 临时会话、纯内存测试、以及不需要跨重启恢复的场景。
///
/// **id 唯一性是契约要求**：`History` 里按 id 定位的路径（如 `upsert_tool`）依赖它。
/// 历史 bug：这里曾返回空串，导致 `upsert_tool` 用 `id == ""` 命中 `inner[0]`
/// （子 agent 里是 system 消息）并把它覆盖成 tool 结果 —— 表现为子 agent
/// 「触顶 → 继续」重放后请求被 400 `tool result's tool id(...) not found` 拒绝。
pub struct InMemoryStore;

impl InMemoryStore {
    pub fn new() -> Self {
        Self
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl ChatHistoryStore for InMemoryStore {
    async fn load(&self) -> Vec<StoreMessage> {
        Vec::new()
    }

    async fn append(&self, _msg: &StoreMessage) -> String {
        // 不落盘，但仍按 trait 契约返回**唯一** id（进程内自增）。
        NEXT_MEMORY_STORE_ID.fetch_add(1, Ordering::Relaxed).to_string()
    }

    async fn update(&self, _id: &str, _msg: &StoreMessage) {
        // 不落盘
    }

    async fn clear(&self) {
        // 不落盘
    }
}
