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

use planned_agent_core::ai::types::Message;

/// 消息级持久化接口，与 `History` 的操作一一对应。
///
/// 构造时由调用方绑定会话；trait 本身无 session 概念。
pub trait ChatHistoryStore: Send + Sync {
    /// 恢复历史（`History::new` 时调用一次，填入内存热数据）。
    fn load(&self) -> Vec<Message>;

    /// 追加一条消息（`push_user` / `push_assistant` / `push_tool` 后调用）。
    fn append(&self, msg: &Message);

    /// 回滚到指定长度（`rollback_to` / `pop_last` / `clean_unclosed` 后调用）。
    ///
    /// 实现方应删除 `sequence_order >= len` 或等效语义的行。
    fn rollback_to(&self, len: usize);

    /// 清空会话（`clear` / `reset_session` 后调用）。
    fn clear(&self);
}

/// 默认内存实现：不持久化，所有操作为空操作。
///
/// 用于子 agent 临时会话、纯内存测试、以及不需要跨重启恢复的场景。
pub struct InMemoryStore;

impl ChatHistoryStore for InMemoryStore {
    fn load(&self) -> Vec<Message> {
        Vec::new()
    }

    fn append(&self, _msg: &Message) {
        // 不落盘
    }

    fn rollback_to(&self, _len: usize) {
        // 不落盘
    }

    fn clear(&self) {
        // 不落盘
    }
}
