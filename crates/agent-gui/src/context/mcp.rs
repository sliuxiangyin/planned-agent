//! MCP 管理器 GUI 适配层

use std::sync::Arc;
use std::time::Duration;

use planned_agent_mcp_rmcp::McpManager;

use crate::config::McpServerConfig as GuiMcpServerConfig;

/// 单次 MCP 连接尝试的最大时长（与 CLI 一致）
const MCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// GUI 层 MCP 上下文
///
/// 组件通过 `use_context::<Resource<Option<Arc<McpContext>>>>()` 获取，
/// 再通过 `ctx.manager.get_all_tools()` 列举 MCP 工具。
pub struct McpContext {
    pub manager: Arc<McpManager>,
}

impl McpContext {
    /// 从 GUI 配置的 MCP server 列表异步初始化（带 10s 超时）
    ///
    /// `connect_all` 内部对每个 server 容错（失败的 server 仅记 warn），因此
    /// 即使全部失败也会返回 Ok；调用方可通过 `InitStatus.mcp.state` 反映
    /// 是否有 server 真正连上。
    pub async fn init(configs: &[GuiMcpServerConfig]) -> anyhow::Result<Self> {
        if configs.is_empty() {
            tracing::info!("MCP 管理器初始化: 无 server 配置");
            return Ok(Self {
                manager: Arc::new(McpManager::new()),
            });
        }

        let mut manager = McpManager::new();
        let core_configs: Vec<_> = configs
            .iter()
            .map(|c| planned_agent_core::types::McpServerConfig {
                name: c.name.clone(),
                server_command: c.server_command.clone(),
                server_args: c.server_args.clone(),
                transport: c.transport.clone(),
                timeout_secs: c.timeout_secs,
                max_retries: c.max_retries,
                is_default: c.is_default,
                tools_filter: c.tools_filter.clone(),
                categories: c.categories.clone(),
            })
            .collect();

        // 套 10s 超时；超时本身不致命（部分 server 也许仍连接上）
        match tokio::time::timeout(MCP_CONNECT_TIMEOUT, manager.connect_all(core_configs)).await {
            Ok(result) => {
                if let Err(e) = result {
                    tracing::warn!("MCP 连接过程出错: {}", e);
                }
            }
            Err(_) => {
                tracing::warn!("MCP 连接超时（{}s）", MCP_CONNECT_TIMEOUT.as_secs());
            }
        }

        let stats = manager.get_connection_status().await;
        for (name, status) in &stats {
            tracing::info!("MCP server '{}': {}", name, status);
        }
        let connected = manager.get_server_names().len();
        tracing::info!("MCP 管理器初始化完成: {}/{} servers connected", connected, configs.len());

        Ok(Self {
            manager: Arc::new(manager),
        })
    }
}