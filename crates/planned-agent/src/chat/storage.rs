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
//!   InMemoryStore 内部转为 `index.to_string()`，SQLite 实现直接返回 UUID。

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
}

impl StoreMessage {
    pub fn new(message: Message, is_error_type: ErrorType) -> Self {
        Self {
            message,
            is_error_type,
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
/// - `InMemoryStore` 返回 `index.to_string()`；
/// - SQLite 实现返回 UUID 主键。
pub trait ChatHistoryStore: Send + Sync {
    /// 恢复历史（`History::new` 时调用一次，填入内存热数据）。
    fn load(&self) -> Vec<StoreMessage>;

    /// 追加一条消息，返回持久化 ID。
    fn append(&self, msg: &StoreMessage) -> String;

    /// 根据 ID 更新消息内容。
    fn update(&self, id: &str, msg: &StoreMessage);

    /// 回滚到指定长度（`rollback_to` / `pop_last` / `clean_unclosed` 后调用）。
    ///
    /// 实现方应删除 `sequence_order >= len` 或等效语义的行。
    fn rollback_to(&self, len: usize);

    /// 清空会话（`clear` / `reset_session` 后调用）。
    fn clear(&self);
}

// ── InMemoryStore ─────────────────────────────────────────────────────────

/// 默认内存实现：不持久化，所有操作为空操作。
///
/// 用于子 agent 临时会话、纯内存测试、以及不需要跨重启恢复的场景。
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

impl ChatHistoryStore for InMemoryStore {
    fn load(&self) -> Vec<StoreMessage> {
        Vec::new()
    }

    fn append(&self, _msg: &StoreMessage) -> String {
        String::new() // 内存实现不实际存储
    }

    fn update(&self, _id: &str, _msg: &StoreMessage) {
        // 不落盘
    }

    fn rollback_to(&self, _len: usize) {
        // 不落盘
    }

    fn clear(&self) {
        // 不落盘
    }
}
