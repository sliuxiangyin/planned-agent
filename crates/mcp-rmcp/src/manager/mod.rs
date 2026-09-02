//! `McpManager` —— MCP 单门面（运行时连接 + 持久化 config/status + 预载/刷新）
//!
//! 对外唯一门面。`struct` 与构造在 `mod.rs`，`impl` 按主题拆到下列子文件：
//! - `routing`  运行时：连接/断开/懒连/工具注入/调用与登记表查询 + `McpManagerTrait`
//! - `config`   服务 CRUD（持久化 config / tools cache）
//! - `status`   连接状态读写（持久化 status）
//! - `views`    config+status join 视图（`load_servers` / `get_server`）
//! - `refresh`  预载缓存工具 / 刷新某 server 工具
//!
//! 读路径（`list_servers` / `get_server` / `server_tools` / 状态）**无副作用、不触发连接**；
//! 连接与拉取只发生在写路径（`refresh_server_tools` / 显式 `connect_server` / `call_tool_auto` 懒连）。

mod config;
mod refresh;
mod routing;
mod status;
mod views;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use planned_agent_core::mcp::types::McpServerConfig;

use crate::bundle::McpBundle;
use crate::client::McpClientImpl;
use crate::config::McpConfigManager;
use crate::storage::{FileMcpStatusStorage, McpConfigStorage, McpStatusStorage};
use crate::tools::ToolManager;

// ═══════════════════════════════════════════════════════════════════════
// 内部可变状态（std::sync::RwLock 支持多读单写）
// ═══════════════════════════════════════════════════════════════════════

struct McpManagerInner {
    /// 已连接的客户端（Arc 允许跨 await 持有引用）
    clients: HashMap<String, Arc<McpClientImpl>>,
    /// 内部工具映射（server → tools），用于 call_tool_auto 路由
    tool_manager: ToolManager,
    /// 服务器配置缓存（用于懒连接）
    server_configs: HashMap<String, McpServerConfig>,
}

impl McpManagerInner {
    fn new() -> Self {
        Self {
            clients: HashMap::new(),
            tool_manager: ToolManager::new(),
            server_configs: HashMap::new(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// McpManager（线程安全 + 懒连接）
// ═══════════════════════════════════════════════════════════════════════

/// MCP 管理器（单门面）
///
/// 使用 `Arc<RwLock<McpManagerInner>>` 实现内部可变性：
/// - `connect_server` / `disconnect_server` 获取 write lock
/// - `call_tool` 获取 read lock 后 clone Arc，释放锁再 await
/// - 支持懒连接：`call_tool_auto` 自动连接未连接的 server
///
/// 同时聚合持久化门面 `bundle`（config + tools cache + 连接状态，后端可插拔），
/// 并提供 `preload_cached_tools` / `refresh_server_tools` 等单门面方法。
pub struct McpManager {
    /// 运行时连接层（多 server 客户端 / 路由表 / 懒连接 config）
    inner: Arc<RwLock<McpManagerInner>>,
    /// 持久化门面（config + tools cache + 连接状态），合并自原 McpBundle
    pub(crate) bundle: McpBundle,
}

impl McpManager {
    /// 默认文件后端门面（CLI 兼容）
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(McpManagerInner::new())),
            bundle: McpBundle::new(
                McpConfigManager::DEFAULT_PATH,
                FileMcpStatusStorage::DEFAULT_PATH,
            ),
        }
    }

    /// 可插拔后端：config / status 各自独立选择（GUI 传 KV、测试传内存）
    pub fn with_backends(
        config_storage: Arc<dyn McpConfigStorage>,
        status_storage: Arc<dyn McpStatusStorage>,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(McpManagerInner::new())),
            bundle: McpBundle::with_storage(config_storage, status_storage),
        }
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}
