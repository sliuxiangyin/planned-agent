//! `agent-gui` 本地 KV 缓存基础设施（sled 后端）
//!
//! 与 [`crate::storage`]（SeaORM+SQLite）正交：
//! - `storage` 负责结构化业务数据（SQL 查询、迁移管理）
//! - `cache` 负责高频 KV 读写场景（AI 响应缓存、工具输出缓存、UI 草稿等）
//!
//! 上层通过 [`crate::context::kv::KvContext`] 消费。

pub mod error;
pub mod mcp_kv_storage;
pub mod mcp_status_kv_storage;
pub mod store;

pub use error::{CacheError, CacheResult};
pub use mcp_kv_storage::KvMcpConfigStorage;
pub use mcp_status_kv_storage::KvMcpStatusStorage;
pub use store::{KvStats, KvStore};
