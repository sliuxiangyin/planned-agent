//! MCP 配置存储 trait —— 调用方按场景实现后注入 [`McpConfigManager`]
//!
//! 设计要点：
//! - **同步 API**：文件、sled 等当前所有后端都是同步 I/O；trait 不引入 `async_trait`
//! - **Send + Sync**：保证 `Arc<dyn McpConfigStorage>` 可跨线程共享（GUI 多 Resource / tokio）
//! - **单一入口**：`McpConfigManager` 内部不再有任何文件 I/O，全部委托 storage
//!
//! 调用方典型用法：
//!
//! ```ignore
//! use std::sync::Arc;
//! use planned_agent_mcp_rmcp::{
//!     McpConfigManager, storage::{McpConfigStorage, FileMcpConfigStorage},
//! };
//!
//! // CLI：默认文件存储（向后兼容）
//! let mgr = McpConfigManager::new("./data/mcp-config.json");
//!
//! // GUI 或测试：注入自定义实现
//! let storage: Arc<dyn McpConfigStorage> = Arc::new(MyKvStorage::new(...));
//! let mgr = McpConfigManager::with_storage(storage);
//! ```

use anyhow::Result;
use planned_agent_core::mcp::types::Tool;

use crate::config::{McpConfigFile, McpServerEntry};

/// MCP 配置存储抽象接口
///
/// 由调用方按场景实现：
/// - **CLI 场景**：[`crate::storage::FileMcpConfigStorage`]（mcp-rmcp 内置）
/// - **GUI 场景**：调用方自实现（如基于 sled KV）
/// - **测试场景**：[`crate::storage::InMemoryMcpConfigStorage`]（mcp-rmcp 内置）
///
/// 所有方法语义对齐原 [`crate::config::McpConfigManager`] 中的同名方法，
/// 实现方需保证：
/// - `load_config` 在底层不存在时返回默认配置（参考 [`McpConfigFile::default`]）
/// - `save_config` 是原子或事务语义（避免半写入状态被读到）
/// - CRUD 操作对不存在的 server 返回明确错误（不要静默忽略）
pub trait McpConfigStorage: Send + Sync {
    // ── 配置根加载 / 保存 ──

    /// 加载完整配置；底层不存在时返回默认配置（已自动持久化）
    fn load_config(&self) -> Result<McpConfigFile>;

    /// 保存完整配置（覆盖式）
    fn save_config(&self, config: &McpConfigFile) -> Result<()>;

    // ── 服务器 CRUD ──

    /// 新增服务器；返回更新后的完整配置
    fn add_server(&self, entry: McpServerEntry) -> Result<McpConfigFile>;

    /// 按名称更新服务器；不存在时返回错误
    fn update_server(&self, name: &str, entry: McpServerEntry) -> Result<McpConfigFile>;

    /// 按名称删除服务器；不存在时返回错误
    fn delete_server(&self, name: &str) -> Result<McpConfigFile>;

    // ── 工具缓存 ──

    /// 将工具列表写入指定服务器的缓存（覆盖已有缓存）
    fn cache_tools(&self, server_name: &str, tools: &[Tool]) -> Result<()>;

    /// 读取指定服务器的缓存工具（无缓存返回空 vec）
    fn get_cached_tools(&self, server_name: &str) -> Vec<Tool>;

    /// 读取所有服务器的缓存工具，返回 `(server_name, tools)`
    fn get_all_cached_tools(&self) -> Vec<(String, Vec<Tool>)>;

    /// 清除指定服务器的缓存工具
    fn clear_cached_tools(&self, server_name: &str) -> Result<()>;

    /// 检查指定服务器是否有缓存工具
    fn has_cached_tools(&self, server_name: &str) -> bool;
}