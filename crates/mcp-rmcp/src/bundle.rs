//! MCP 统一门面 [`McpBundle`] —— 把 list（config + tools cache）和 status 聚合暴露
//!
//! ## 设计动机
//!
//! `McpConfigStorage`（管 servers / tools cache）和 `McpStatusStorage`（管连接状态）
//! 是两个**独立**的存储 trait，调用方可以分别为它们选择后端（File / KV / DB / 内存）。
//!
//! 但每次让外部使用方手动 "load config → load status → 按 name 配对" 极易出错。
//! [`McpBundle`] 把这两个存储捏在一起：
//!
//! - **统一视图读取**：`load_servers()` 一次返回 `Vec<McpServerView>`（已 join）
//! - **联动写操作**：`delete_server()` / `fetch_and_cache_tools()` 内部消化状态联动
//! - **减少心智**：调用方只持有一个 bundle，不再需要协调两个 manager
//!
//! ## 层级关系
//!
//! ```text
//!   McpBundle (本文件)
//!     ├─ McpConfigManager (config 侧，内部组件)
//!     └─ Arc<dyn McpStatusStorage> (status 侧，直接持有 trait obj)
//! ```
//!
//! bundle 是 mcp-rmcp 对外暴露的**唯一推荐门面**；
//! 内部组件（McpConfigManager / 两个 storage trait）仍可独立访问，
//! 用于 CLI / 高级场景。

use std::sync::Arc;

use anyhow::Result;
use planned_agent_core::types::ConnectionError;

use crate::config::{McpConfigFile, McpConfigManager, McpRefreshError, McpServerEntry};
use crate::storage::{McpStatusStorage, ServerStatus};
use planned_agent_core::types::Tool;

// ═══════════════════════════════════════════════════════════════════════
// 统一视图类型
// ═══════════════════════════════════════════════════════════════════════

/// 单个 MCP server 的"消费侧视图"——把 config 和 status 合并暴露
///
/// 这是 GUI [`mcp::list_page`](crate) 真正需要的数据形态：
/// 不再让 UI 侧手动按 name 拼两个 HashMap。
#[derive(Debug, Clone)]
pub struct McpServerView {
    /// 来自 `McpConfigStorage`（list / tools cache）
    pub config: McpServerEntry,
    /// 来自 `McpStatusStorage`（最近一次 connect 结果）
    ///
    /// 缺失时为 `None`（从未连接过 / 状态已被清理）。
    pub status: Option<ServerStatus>,
}

impl McpServerView {
    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn has_cached_tools(&self) -> bool {
        !self.config.cached_tools.is_empty()
    }

    pub fn cached_tools_count(&self) -> usize {
        self.config.cached_tools.len()
    }

    pub fn command_str(&self) -> String {
        format!("{} {}", self.config.server_command, self.config.server_args.join(" "))
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 工具函数
// ═══════════════════════════════════════════════════════════════════════

/// 按字符数截断字符串（按 Unicode scalar value 而非字节），避免切半 UTF-8 字符。
///
/// 末尾加 `…` 表示截断发生。
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_chars * 4));
    for (i, c) in s.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════
// McpBundle
// ═══════════════════════════════════════════════════════════════════════

/// MCP 统一门面：聚合 list / tools cache 与 status 两层存储，对外暴露统一接口
///
/// ## 构造
///
/// 两个 trait 实现由调用方独立传入——list 和 status **可任意搭配 backend**：
///
/// ```ignore
/// // 全部用 KV
/// let bundle = McpBundle::with_storage(
///     Arc::new(KvMcpConfigStorage::new(store.clone())),
///     Arc::new(KvMcpStatusStorage::new(store)),
/// );
///
/// // config 用文件，status 用 KV（完全异构）
/// let bundle = McpBundle::with_storage(
///     Arc::new(FileMcpConfigStorage::new("./data/mcp-config.json")),
///     Arc::new(KvMcpStatusStorage::new(store)),
/// );
/// ```
pub struct McpBundle {
    config_manager: McpConfigManager,
    status_storage: Arc<dyn McpStatusStorage>,
}

impl McpBundle {
    /// **便捷构造**（向后兼容）：两个都用文件存储
    ///
    /// CLI 场景：`McpBundle::new("./data/mcp-config.json", "./data/mcp-status.json")`
    pub fn new(config_path: &str, status_path: &str) -> Self {
        Self::with_storage(
            Arc::new(crate::storage::FileMcpConfigStorage::new(config_path)),
            Arc::new(crate::storage::FileMcpStatusStorage::new(status_path)),
        )
    }

    /// **DI 构造**：调用方注入任意 [`McpConfigStorage`] + [`McpStatusStorage`] 实现
    ///
    /// 两个 trait 实现**完全独立选择**（可同可异）。
    pub fn with_storage(
        config_storage: Arc<dyn crate::storage::McpConfigStorage>,
        status_storage: Arc<dyn McpStatusStorage>,
    ) -> Self {
        Self {
            config_manager: McpConfigManager::with_storage(config_storage),
            status_storage,
        }
    }

    // ── 内部组件访问（CLI / 高级用法） ──

    /// 访问 config 侧 manager
    pub fn config_manager(&self) -> &McpConfigManager {
        &self.config_manager
    }

    /// 访问 status 侧 storage
    pub fn status_storage(&self) -> &Arc<dyn McpStatusStorage> {
        &self.status_storage
    }

    // ═════════════════════════════════════════════════════════════════
    // 统一视图读取（核心收益：UI 不再手动 join）
    // ═════════════════════════════════════════════════════════════════

    /// 加载所有 server 的视图（已 join config + status）
    ///
    /// 内部流程：
    /// 1. `config_manager.load_config()` → `Vec<McpServerEntry>`
    /// 2. `status_storage.load_all()` → `HashMap<name, ServerStatus>`
    /// 3. 按 name 配对，返回 `Vec<McpServerView>`
    pub fn load_servers(&self) -> Result<Vec<McpServerView>> {
        let cfg = self.config_manager.load_config()?;
        let statuses = self
            .status_storage
            .load_all()?
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();

        Ok(cfg
            .servers
            .into_iter()
            .map(|config| {
                let status = statuses.get(&config.name).cloned();
                McpServerView { config, status }
            })
            .collect())
    }

    /// 按 name 读取单个 server 的视图
    pub fn get_server(&self, name: &str) -> Result<Option<McpServerView>> {
        let cfg = self.config_manager.load_config()?;
        let entry = match cfg.servers.into_iter().find(|s| s.name == name) {
            Some(e) => e,
            None => return Ok(None),
        };
        let status = self.status_storage.get(name)?;
        Ok(Some(McpServerView { config: entry, status }))
    }

    // ═════════════════════════════════════════════════════════════════
    // 联动写操作
    // ═════════════════════════════════════════════════════════════════

    /// 删除 server（config + status 联动清理）
    ///
    /// - config 删除失败 → 抛错
    /// - status 删除失败 → 仅 `tracing::warn!`，**不**影响 config 已生效的结果
    ///
    /// 返回 **删除后的** [`McpConfigFile`]（与 [`McpConfigManager::delete_server`] 行为一致），
    /// 供调用方更新本地 signal。
    pub fn delete_server(&self, name: &str) -> Result<McpConfigFile> {
        let cfg = self.config_manager.delete_server(name)?;
        if let Err(e) = self.status_storage.delete(name) {
            tracing::warn!(
                "删除 MCP status 失败（不影响 config 删除）: server='{}', err={}",
                name,
                e
            );
        }
        Ok(cfg)
    }

    // ═════════════════════════════════════════════════════════════════
    // 复合操作：fetch_and_cache_tools 内聚 status 写入
    // ═════════════════════════════════════════════════════════════════

    /// **刷新工具**：连接 MCP → 拉取 → 缓存 → **自动记录 status**
    ///
    /// 内部流程：
    /// 1. 调用 [`McpConfigManager::fetch_and_cache_tools`] 完成连接 / 拉取 / 缓存
    /// 2. 成功 → 自动 `status_storage.record(Ready(n))`
    /// 3. 失败（且带结构化 `ConnectionError`）→ 自动 `status_storage.record(Failed(kind, msg))`
    ///
    /// 调用方**不需要**再单独调 `record_status`，bundle 已统一处理。
    pub async fn fetch_and_cache_tools(
        &self,
        server_entry: &McpServerEntry,
    ) -> std::result::Result<(String, Vec<Tool>), McpRefreshError> {
        match self.config_manager.fetch_and_cache_tools(server_entry).await {
            Ok((name, tools)) => {
                // 成功：记录 Ready
                let status = ServerStatus::ready(tools.len() as u32, ServerStatus::now());
                if let Err(e) = self.status_storage.record(&name, status) {
                    tracing::warn!(
                        "持久化 MCP Ready status 失败: server='{}', err={}",
                        name,
                        e
                    );
                }
                Ok((name, tools))
            }
            Err(refresh_err) => {
                // 失败：若有结构化错误，记录 Failed（截断到 200 字符）
                if let Some(ref conn_err) = refresh_err.connection_error {
                    let msg = truncate_chars(&conn_err.message(), 200);
                    let status = ServerStatus::failed(
                        conn_err.kind_str(),
                        msg,
                        ServerStatus::now(),
                    );
                    if let Err(e) = self.status_storage.record(&server_entry.name, status) {
                        tracing::warn!(
                            "持久化 MCP Failed status 失败: server='{}', err={}",
                            server_entry.name,
                            e
                        );
                    }
                }
                Err(refresh_err)
            }
        }
    }

    // ═════════════════════════════════════════════════════════════════
    // status 显式接口（CLI / 高级用法）
    // ═════════════════════════════════════════════════════════════════

    /// 记录一次连接状态（覆盖式）
    pub fn record_status(&self, name: &str, status: ServerStatus) -> Result<()> {
        match self.status_storage.record(name, status) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("持久化 MCP status 失败: server='{}', err={}", name, e);
                Ok(())
            }
        }
    }

    /// 读取单个 server 的最近状态
    pub fn get_status(&self, name: &str) -> Result<Option<ServerStatus>> {
        self.status_storage.get(name)
    }

    /// 加载所有 server 的最近状态
    pub fn load_all_statuses(&self) -> Result<Vec<(String, ServerStatus)>> {
        self.status_storage.load_all()
    }

    /// 删除指定 server 的状态
    pub fn delete_status(&self, name: &str) -> Result<()> {
        self.status_storage.delete(name)
    }

    /// 检查指定 server 是否有状态记录
    pub fn has_status(&self, name: &str) -> bool {
        self.status_storage.has_status(name)
    }

    /// 把 `ConnectionError` 直接落库（CLI 等场景的便捷方法）
    pub fn record_failure(&self, server_name: &str, conn_err: &ConnectionError) {
        let msg = truncate_chars(&conn_err.message(), 200);
        let status = ServerStatus::failed(conn_err.kind_str(), msg, ServerStatus::now());
        let _ = self.record_status(server_name, status);
    }

    // ── 转发到 config_manager 的便捷方法 ──

    /// 加载完整 config（绕过 join，给 CLI 等需要原始数据的场景）
    pub fn load_config(&self) -> Result<McpConfigFile> {
        self.config_manager.load_config()
    }
}

impl Clone for McpBundle {
    fn clone(&self) -> Self {
        Self {
            config_manager: self.config_manager.clone(),
            status_storage: self.status_storage.clone(),
        }
    }
}