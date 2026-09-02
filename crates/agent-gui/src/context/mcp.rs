//! MCP 管理器 GUI 适配层
//!
//! 单门面：`McpContext` 只持有 `McpManager`（运行时连接 + 持久化 config/status + 预载/刷新）。
//! 冷启动 `init` 预载缓存工具进 McpManager 路由表（不连接任何 server）；main.rs 随后
//! `set_mcp_manager` 把 MCP 工具统一注册进 ToolRegistry（唯一入口，避免重复注册）。
//! 刷新/删除等由本层转发 `McpManager` 并同步 `ToolRegistry`（分类映射收在 tool-manager 内）。

use dioxus::prelude::{ReadableExt, Signal, WritableExt};
use std::sync::Arc;

use planned_agent_core::mcp::types::ConnectionError;
use planned_agent_mcp_rmcp::{
    storage::{FileMcpConfigStorage, FileMcpStatusStorage, McpConfigStorage, McpStatusStorage},
    McpManager, McpServerView, ServerStatus,
};
use planned_agent_tool_manager::ToolRegistry;

use crate::cache::{KvMcpConfigStorage, KvMcpStatusStorage};
use crate::context::KvContext;

/// MCP 配置/状态变更通知器
///
/// 任何对 MCP 数据的写入后都应 `bump()`，让 [`McpListPage`](crate::pages::mcp::McpListPage)
/// 等 UI 监听者重新从 [`McpManager::list_servers`] 加载视图。
///
/// 作为独立 Dioxus context 提供，与 [`McpContext`] 解耦：
/// - [`McpContext`] 在 Resource 中可能为 `None`（init 失败）
/// - notifier 永远可用，任何位置 `bump()` 都可触发全 UI 同步
#[derive(Clone, Copy)]
pub struct McpChangeNotifier {
    version: Signal<u64>,
}

impl McpChangeNotifier {
    /// 创建一个新的变更通知器（绑定到一个新的 Signal）
    pub fn new() -> Self {
        Self {
            version: Signal::new(0),
        }
    }

    /// 从已有的 Signal 创建（用于在 use_signal 已存在的情况下绑定）
    pub fn from_signal(version: Signal<u64>) -> Self {
        Self { version }
    }

    /// 自增版本号，触发所有 use_effect 订阅者重新执行
    pub fn bump(&self) {
        // Signal 是 Copy；复制出可变副本后写入
        let mut signal = self.version;
        let cur = signal.cloned();
        signal.set(cur + 1);
    }

    /// 当前版本号（只读，便于调试）
    pub fn version(&self) -> u64 {
        self.version.cloned()
    }
}

/// GUI 层 MCP 上下文
///
/// 组件通过 `use_context::<Resource<Option<Arc<McpContext>>>>()` 获取。
pub struct McpContext {
    /// MCP 单门面：运行时连接 + 持久化 config/status + 预载/刷新
    pub manager: Arc<McpManager>,
}

impl McpContext {
    /// 初始化：按场景选择存储后端 → 加载配置 → 创建 McpManager（不连接任何服务器）
    ///
    /// ## 存储后端选择（两个 trait 完全独立选择）
    ///
    /// - `kv = Some(...)`：使用 [`KvMcpConfigStorage`] + [`KvMcpStatusStorage`]
    /// - `kv = None`：**降级**到 [`FileMcpConfigStorage`] + [`FileMcpStatusStorage`]，
    ///   保留 CLI / 文件兼容行为
    pub async fn init(kv: Option<Arc<KvContext>>) -> anyhow::Result<Self> {
        // 1. config 存储（list + tools cache）
        let config_storage: Arc<dyn McpConfigStorage> = match kv.as_ref() {
            Some(kv_ctx) => {
                tracing::info!("MCP 配置使用 KV 后端 (tree='mcp_config')");
                Arc::new(KvMcpConfigStorage::new(kv_ctx.store.clone()))
            }
            None => {
                tracing::warn!(
                    "MCP 配置降级到文件后端: {}",
                    FileMcpConfigStorage::default_path()
                );
                Arc::new(FileMcpConfigStorage::new(
                    FileMcpConfigStorage::default_path(),
                ))
            }
        };

        // 2. status 存储（**完全独立**选择后端，可与 config 异构）
        let status_storage: Arc<dyn McpStatusStorage> = match kv.as_ref() {
            Some(kv_ctx) => {
                tracing::info!("MCP 状态使用 KV 后端 (tree='mcp_status')");
                Arc::new(KvMcpStatusStorage::new(kv_ctx.store.clone()))
            }
            None => {
                tracing::warn!(
                    "MCP 状态降级到文件后端: {}",
                    FileMcpStatusStorage::DEFAULT_PATH
                );
                Arc::new(FileMcpStatusStorage::new(
                    FileMcpStatusStorage::DEFAULT_PATH,
                ))
            }
        };

        // 3. 单门面 McpManager：聚合可插拔 config/status 后端
        let manager = McpManager::with_backends(config_storage, status_storage);

        // 4. 预载缓存工具进运行时路由表（内部记 Ready；不连接任何 server）
        manager.preload_cached_tools()?;

        // 5. 打印每个 server 的缓存情况
        let mcp_config = manager.load_config()?;
        let total_cached: usize = mcp_config
            .servers
            .iter()
            .map(|s| s.cached_tools.len())
            .sum();
        tracing::info!(
            "MCP 配置加载完成: {} 个 server，共 {} 个缓存工具",
            mcp_config.servers.len(),
            total_cached,
        );
        for server in &mcp_config.servers {
            tracing::info!(
                "MCP server '{}': {} 个缓存工具",
                server.name,
                server.cached_tools.len()
            );
        }

        Ok(Self {
            manager: Arc::new(manager),
        })
    }

    /// 加载所有 server 的"统一视图"（config + status 已 join）
    ///
    /// 这是 GUI `list_page` 的核心入口：一次拿到 `Vec<McpServerView>`。
    pub fn load_servers(&self) -> Vec<McpServerView> {
        self.manager.list_servers().unwrap_or_else(|e| {
            tracing::warn!("加载 MCP servers 失败，按空启动: {}", e);
            Vec::new()
        })
    }

    /// 加载所有 server 的最近连接状态
    pub fn load_all_statuses(&self) -> Vec<(String, ServerStatus)> {
        self.manager.list_status().unwrap_or_else(|e| {
            tracing::warn!("加载 MCP 状态失败，按空启动: {}", e);
            Vec::new()
        })
    }

    /// 删除 server（config + status 联动清理）
    pub fn delete_server(
        &self,
        name: &str,
    ) -> anyhow::Result<planned_agent_mcp_rmcp::config::McpConfigFile> {
        self.manager.delete_server(name)
    }

    /// 刷新指定服务器工具：连接 → 拉取 → 缓存 → 更新路由 → 自动记录状态 → 同步 ToolRegistry
    ///
    /// 返回 `Ok(server_name, tools_count)` 或 `Err(anyhow::Error, Option<ConnectionError>)>`，
    /// 其中 `ConnectionError` 携带失败阶段的结构化分类（Timeout / Spawn / Handshake）。
    pub async fn refresh_tools(
        &self,
        server_name: &str,
        tool_registry: &Arc<ToolRegistry>,
    ) -> Result<(String, usize), (anyhow::Error, Option<ConnectionError>)> {
        // 1. manager 刷新：连→拉→缓存→更新运行时路由表；内部自动 record Ready/Failed
        self.manager
            .refresh_server_tools(server_name)
            .await
            .map_err(|e| (e.source, e.connection_error))?;

        // 2. 同步 ToolRegistry：卸旧 → 从 McpManager 登记表重注册该 server（分类映射在 tool-manager 内统一）
        let count = tool_registry
            .sync_mcp_server(server_name)
            .map_err(|e| (e, None))?;

        Ok((server_name.to_string(), count))
    }
}
