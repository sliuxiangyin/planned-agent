//! MCP 管理器 GUI 适配层
//!
//! 启动时从存储加载配置（不连接任何 MCP 服务器）。
//! 缓存工具的注册通过 `register_cached_tools()` 在 use_effect 中完成。
//! 工具刷新、连接管理等操作通过 McpContext 暴露给 UI 层。
//!
//! 工具注册时同步更新两处：
//!   1. ToolRegistry   — 全局注册表（给 LLM 选工具 + 设置页展示）
//!   2. McpManager     — 内部 ToolManager（给 call_tool_auto 路由 + 懒连接）
//!
//! ## 持久化与 bundle 设计
//!
//! 本层只持有一个 [`McpBundle`]，由它统一聚合：
//!   - `McpConfigStorage`（list + tools cache，独立选择后端）
//!   - `McpStatusStorage`（连接状态，独立选择后端）
//!
//! 调用方通过 `bundle.load_servers()` 一次拿到 config + status 已 join 的视图，
//! 不再需要在 UI 侧手动按 name 配对。
//!
//! ## 变更通知
//!
//! 任何对 MCP 配置/状态的写入（add / update / delete / refresh）都应 `bump()` 一下
//! [`McpChangeNotifier`]，让 list_page 等监听者重新加载视图。
//!
//! `McpChangeNotifier` 作为独立 Dioxus context 提供，与 `McpContext` 解耦：
//! `McpContext` 在 Resource 中可能为 None，但 notifier 永远可用。

use dioxus::prelude::{ReadableExt, Signal, WritableExt};
use std::sync::Arc;

use planned_agent_mcp_rmcp::{
    storage::{FileMcpConfigStorage, FileMcpStatusStorage, McpConfigStorage, McpStatusStorage},
    McpBundle, McpManager, McpServerView, ServerStatus,
};
use planned_agent_core::tool_registry::ToolCategory;
use planned_agent_core::mcp::types::{ConnectionError, Tool};
use planned_agent_mcp_rmcp::config::McpServerEntry;
use planned_agent_tool_manager::ToolRegistry;

use crate::cache::{KvMcpConfigStorage, KvMcpStatusStorage};
use crate::context::KvContext;

/// 将字符串分类转为 ToolCategory 枚举
fn parse_categories(raw: &Option<Vec<String>>) -> Vec<ToolCategory> {
    raw.as_ref()
        .map(|cats| {
            cats.iter()
                .filter_map(|s| match s.as_str() {
                    "Browser" => Some(ToolCategory::Browser),
                    "File" => Some(ToolCategory::File),
                    "Text" => Some(ToolCategory::Text),
                    "Data" => Some(ToolCategory::Data),
                    "System" => Some(ToolCategory::System),
                    "Device" => Some(ToolCategory::Device),
                    "Dev" => Some(ToolCategory::Dev),
                    "Utility" => Some(ToolCategory::Utility),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_else(|| vec![ToolCategory::Utility])
}

/// 将工具列表注册到 ToolRegistry（返回注册数）
fn register_tools_to_registry(
    tool_registry: &ToolRegistry,
    server_name: &str,
    tools: &[Tool],
    categories: &[ToolCategory],
) -> usize {
    let mut count = 0;
    for tool in tools {
        let metadata = planned_agent_tool_manager::types::ToolMetadata {
            source: planned_agent_core::tool_registry::ToolSource::Mcp {
                server_name: server_name.to_string(),
            },
            categories: categories.to_vec(),
            enabled: true,
            priority: 100,
            tags: vec![],
            created_at: chrono::Utc::now(),
            version: None,
        };
        tool_registry.register_tool(tool.clone(), metadata);
        count += 1;
    }
    count
}

/// MCP 配置/状态变更通知器
///
/// 任何对 MCP 数据的写入后都应 `bump()`，让 [`McpListPage`](crate::pages::mcp::McpListPage)
/// 等 UI 监听者重新从 [`McpBundle::load_servers`] 加载视图。
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
        Self { version: Signal::new(0) }
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
    /// 运行时连接层（管多 server 路由 / 懒连接 / tool 调用）
    pub manager: Arc<McpManager>,
    /// 持久化门面（统一聚合 config + status）
    pub bundle: McpBundle,
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

        // 3. bundle 聚合两个 storage（独立 trait，独立后端，统一对外接口）
        let bundle = McpBundle::with_storage(config_storage, status_storage);

        // 4. 加载并记录初始 config（不连接任何 server）
        let mcp_config = bundle.load_config()?;
        let total_cached: usize = mcp_config.servers.iter().map(|s| s.cached_tools.len()).sum();

        tracing::info!(
            "MCP 配置加载完成: {} 个 server，共 {} 个缓存工具",
            mcp_config.servers.len(),
            total_cached,
        );

        for server in &mcp_config.servers {
            if server.cached_tools.is_empty() {
                tracing::info!(
                    "MCP server '{}': 无缓存工具（可通过设置页'刷新工具'获取）",
                    server.name
                );
            } else {
                tracing::info!(
                    "MCP server '{}': {} 个缓存工具待注册",
                    server.name,
                    server.cached_tools.len()
                );
            }
        }

        Ok(Self {
            manager: Arc::new(McpManager::new()),
            bundle,
        })
    }

    /// 加载所有 server 的"统一视图"（config + status 已 join）
    ///
    /// 这是 GUI `list_page` 的核心入口：一次拿到 `Vec<McpServerView>`，
    /// 不再需要在 UI 侧手动按 name 配对两个 HashMap。
    pub fn load_servers(&self) -> Vec<McpServerView> {
        self.bundle.load_servers().unwrap_or_else(|e| {
            tracing::warn!("加载 MCP servers 失败，按空启动: {}", e);
            Vec::new()
        })
    }

    /// 加载所有 server 的最近连接状态（兼容接口，等价于 `load_servers()` 的 status 部分）
    ///
    /// 保留这个方法以便现有调用方平滑迁移。
    pub fn load_all_statuses(&self) -> Vec<(String, ServerStatus)> {
        self.bundle.load_all_statuses().unwrap_or_else(|e| {
            tracing::warn!("加载 MCP 状态失败，按空启动: {}", e);
            Vec::new()
        })
    }

    /// 删除 server（config + status 联动清理）
    ///
    /// 返回删除后的 config（与 [`McpBundle::delete_server`] 一致）。
    pub fn delete_server(&self, name: &str) -> anyhow::Result<planned_agent_mcp_rmcp::config::McpConfigFile> {
        self.bundle.delete_server(name).map_err(Into::into)
    }

    /// 将缓存工具注册到 ToolRegistry + 同步到 McpManager 内部 ToolManager
    ///
    /// 返回注册的工具总数
    ///
    /// **副作用**：缓存工具存在即视为 Ready(n)，会写入 status
    /// （注意：这不是走 bundle 的 fetch_and_cache_tools 路径，因为没有真实连接；
    /// 这里只是冷启动从 config 加载 cached_tools，所以单独记录 Ready）
    pub fn register_cached_tools(&self, tool_registry: &Arc<ToolRegistry>) -> usize {
        let mcp_config = match self.bundle.load_config() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("加载 MCP 配置失败，跳过缓存工具注册: {}", e);
                return 0;
            }
        };

        let mut total = 0usize;

        for server in &mcp_config.servers {
            if server.cached_tools.is_empty() {
                continue;
            }

            let categories = parse_categories(&server.categories);

            // 1. 注册到 ToolRegistry（给 LLM 选工具 + 设置页展示）
            let tools: Vec<Tool> = server.cached_tools.iter().map(|e| e.to_tool()).collect();
            total += register_tools_to_registry(tool_registry, &server.name, &tools, &categories);

            // 2. 同步到 McpManager 内部 ToolManager（给 call_tool_auto 路由 + 懒连接）
            let core_config = server.to_core_config();
            self.manager.set_server_tools_with_config(&server.name, tools, core_config);

            // 3. 同步 status：缓存工具存在即视为 Ready(n)（用于冷启动可见性）
            let status = ServerStatus::ready(server.cached_tools.len() as u32, ServerStatus::now());
            let _ = self.bundle.record_status(&server.name, status);
        }

        if total > 0 {
            tracing::info!("从缓存注册 {} 个 MCP 工具（ToolRegistry + McpManager 已同步）", total);
        }
        total
    }

    /// 刷新指定服务器的工具：连接 → 拉取 → 缓存 → 断开 → 更新 ToolRegistry + McpManager
    ///
    /// 返回 `Ok(server_name, tools_count)` 或 `Err(anyhow::Error, Option<ConnectionError>)`，
    /// 其中 `ConnectionError` 携带失败阶段的结构化分类（Timeout / Spawn / Handshake）。
    ///
    /// **副作用**：通过 [`McpBundle::fetch_and_cache_tools`] 自动写入 status
    /// （成功 → Ready(n)，失败 → Failed(kind, msg)），调用方**无需**再手动 record。
    pub async fn refresh_tools(
        &self,
        server_name: &str,
        tool_registry: &Arc<ToolRegistry>,
    ) -> Result<(String, usize), (anyhow::Error, Option<ConnectionError>)> {
        tracing::info!("刷新refresh_tools");
        // 1. 找 entry
        let config = self
            .bundle
            .load_config()
            .map_err(|e| (e, None))?;
        let server_entry: McpServerEntry = config
            .servers
            .iter()
            .find(|s| s.name == server_name)
            .ok_or_else(|| {
                (
                    anyhow::anyhow!("MCP 服务器不存在: {}", server_name),
                    None,
                )
            })?
            .clone();

        // 2. 走 bundle 的 fetch_and_cache_tools（自动 record Ready/Failed）
        let (name, tools) = self
            .bundle
            .fetch_and_cache_tools(&server_entry)
            .await
            .map_err(|e| (e.source, e.connection_error))?;

        // 3. 更新 ToolRegistry（卸载旧 → 注册新）
        let _ = tool_registry.unregister_mcp_server_tools(&name);
        let categories = parse_categories(&server_entry.categories);
        let count = register_tools_to_registry(tool_registry, &name, &tools, &categories);

        // 4. 同步到 McpManager 内部 ToolManager（确保 call_tool_auto 路由正确）
        self.manager.set_server_tools(&name, tools);

        tracing::info!(
            "刷新完成: server '{}' → {} 个工具（ToolRegistry + McpManager 已同步）",
            name,
            count
        );
        Ok((name, count))
    }
}