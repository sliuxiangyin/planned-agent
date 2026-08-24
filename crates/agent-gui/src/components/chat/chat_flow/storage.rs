//! 聊天消息持久化抽象。
//!
//! `ChatStorage` trait 定义了聊天消息的 CRUD 接口，
//! 实现方决定存储后端（SQLite / 内存 / 远程 API）。

use async_trait::async_trait;

use super::types::ChatMessage;

/// 聊天消息持久化抽象。
///
/// `ChatSignals` 通过 `Signal<Option<Arc<dyn ChatStorage>>>` 持有。
#[async_trait]
pub trait ChatStorage: Send + Sync {
    /// 持久化一条聊天消息（异步，内部自行 spawn）。
    async fn persist_message(&self, plan_id: &str, msg: &ChatMessage);

    /// 按 plan_id 加载历史消息（按 sequence_order 正序）。
    async fn load_messages(&self, plan_id: &str) -> Vec<ChatMessage>;

    /// 按 plan_id 删除全部历史消息。
    async fn delete_messages(&self, plan_id: &str);
}

/// 空操作存储实现，用于 Context 占位（storage ready 前）。
pub struct DummyStorage;

#[async_trait]
impl ChatStorage for DummyStorage {
    async fn persist_message(&self, _plan_id: &str, _msg: &ChatMessage) {}
    async fn load_messages(&self, _plan_id: &str) -> Vec<ChatMessage> {
        Vec::new()
    }
    async fn delete_messages(&self, _plan_id: &str) {}
}
