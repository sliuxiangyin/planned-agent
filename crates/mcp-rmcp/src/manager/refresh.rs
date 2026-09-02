//! `McpManager` 持久化侧 impl —— 预载缓存工具 / 刷新某 server 工具
//!
//! - `preload_cached_tools`：冷启动把缓存工具喂进运行时路由表（**不连接**）。
//! - `refresh_server_tools`：连接 → list_tools → 缓存 → 更新路由表（真正连接）。

use anyhow::Result;
use planned_agent_core::mcp::types::Tool;

use crate::config::McpRefreshError;
use crate::storage::ServerStatus;

use super::McpManager;

impl McpManager {
    /// **预载缓存工具进内部路由表**（不连接任何 server）
    ///
    /// 遍历持久化 config，把每个「有 cached_tools」的 server 的工具与其 config
    /// 一并喂给运行时路由表（供 `call_tool_auto` 懒连接路由），并记录 Ready(n)。
    ///
    /// 这是冷启动装配的一步：工具 schema 来自缓存，真实连接留到首次调用时才懒建。
    pub fn preload_cached_tools(&self) -> Result<usize> {
        let cfg = self.bundle.load_config()?;
        let mut total = 0usize;

        for server in &cfg.servers {
            if server.cached_tools.is_empty() {
                continue;
            }
            let tools: Vec<Tool> = server.cached_tools.iter().map(|e| e.to_tool()).collect();
            self.set_server_tools_with_config(&server.name, tools, server.to_core_config());
            total += server.cached_tools.len();

            // 缓存工具存在即视为 Ready(n)（用于冷启动可见性；与既有 GUI 语义一致）
            let status = ServerStatus::ready(server.cached_tools.len() as u32, ServerStatus::now());
            let _ = self.bundle.record_status(&server.name, status);
        }

        if total > 0 {
            tracing::info!(
                "预载 {} 个缓存 MCP 工具进运行时路由表（未连接任何 server）",
                total
            );
        }
        Ok(total)
    }

    /// **刷新指定 server 的工具**：连接 → list_tools → 缓存 → 更新路由表
    ///
    /// - 走 `McpBundle::fetch_and_cache_tools`，内部自动缓存并记录
    ///   `Ready(n)` / `Failed(kind, msg)`，调用方无需手动 record。
    /// - 成功后把新工具同步进运行时路由表（`call_tool_auto` 可用）。
    /// - 失败返回结构化 `McpRefreshError`（含可选 `ConnectionError`），不阻塞保存流程。
    pub async fn refresh_server_tools(
        &self,
        server_name: &str,
    ) -> std::result::Result<(String, usize), McpRefreshError> {
        let cfg = self.bundle.load_config().map_err(|source| McpRefreshError {
            source,
            connection_error: None,
        })?;
        let entry = cfg
            .servers
            .iter()
            .find(|s| s.name == server_name)
            .cloned()
            .ok_or_else(|| McpRefreshError {
                source: anyhow::anyhow!("MCP 服务器不存在: {}", server_name),
                connection_error: None,
            })?;

        let (name, tools) = self.bundle.fetch_and_cache_tools(&entry).await?;
        let count = tools.len();
        self.set_server_tools(&name, tools);
        tracing::info!(
            "刷新完成: server '{}' → {} 个工具（已缓存 + 更新运行时路由表）",
            name,
            count
        );
        Ok((name, count))
    }
}
