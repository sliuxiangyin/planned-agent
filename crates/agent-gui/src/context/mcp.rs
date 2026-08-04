//! MCP 管理器 GUI 适配层
//!
//! 启动时从 mcp-config.json 加载配置（不连接任何 MCP 服务器）。
//! 缓存工具的注册通过 `register_cached_tools()` 在 use_effect 中完成。
//! 工具刷新、连接管理等操作通过 McpContext 暴露给 UI 层。
//!
//! 工具注册时同步更新两处：
//!   1. ToolRegistry   — 全局注册表（给 LLM 选工具 + 设置页展示）
//!   2. McpManager     — 内部 ToolManager（给 call_tool_auto 路由 + 懒连接）

use std::sync::Arc;

use planned_agent_mcp_rmcp::{McpConfigManager, McpManager};
use planned_agent_core::tool_registry::ToolCategory;
use planned_agent_core::types::Tool;
use planned_agent_tool_manager::ToolRegistry;
use planned_agent_mcp_rmcp::config::McpServerEntry;

/// 将字符串分类转为 ToolCategory 枚举
fn parse_categories(raw: &Option<Vec<String>>) -> Vec<ToolCategory> {
    raw.as_ref()
        .map(|cats| {
            cats.iter().filter_map(|s| match s.as_str() {
                "Browser" => Some(ToolCategory::Browser),
                "File" => Some(ToolCategory::File),
                "Text" => Some(ToolCategory::Text),
                "Data" => Some(ToolCategory::Data),
                "System" => Some(ToolCategory::System),
                "Device" => Some(ToolCategory::Device),
                "Dev" => Some(ToolCategory::Dev),
                "Utility" => Some(ToolCategory::Utility),
                _ => None,
            }).collect()
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

/// GUI 层 MCP 上下文
///
/// 组件通过 `use_context::<Resource<Option<Arc<McpContext>>>>()` 获取。
pub struct McpContext {
    pub manager: Arc<McpManager>,
    pub config_manager: McpConfigManager,
}

impl McpContext {
    /// 初始化：加载 mcp-config.json，创建 McpManager（不连接任何服务器）
    pub async fn init() -> anyhow::Result<Self> {
        let config_manager = McpConfigManager::new(McpConfigManager::DEFAULT_PATH);

        let mcp_config = config_manager.load_config()?;
        let total_cached: usize = mcp_config.servers.iter().map(|s| s.cached_tools.len()).sum();

        tracing::info!(
            "MCP 配置加载完成: {} 个 server，共 {} 个缓存工具（来自 {}）",
            mcp_config.servers.len(),
            total_cached,
            McpConfigManager::DEFAULT_PATH
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
            config_manager,
        })
    }

    /// 将缓存工具注册到 ToolRegistry + 同步到 McpManager 内部 ToolManager
    ///
    /// 返回注册的工具总数
    pub fn register_cached_tools(&self, tool_registry: &Arc<ToolRegistry>) -> usize {
        let mcp_config = match self.config_manager.load_config() {
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
        }

        if total > 0 {
            tracing::info!("从缓存注册 {} 个 MCP 工具（ToolRegistry + McpManager 已同步）", total);
        }
        total
    }

    /// 刷新指定服务器的工具：连接 → 拉取 → 缓存 → 断开 → 更新 ToolRegistry + McpManager
    ///
    /// 返回 (server_name, tools_count)
    pub async fn refresh_tools(
        &self,
        server_name: &str,
        tool_registry: &Arc<ToolRegistry>,
    ) -> anyhow::Result<(String, usize)> {
        let config = self.config_manager.load_config()?;
        let server_entry: McpServerEntry = config.servers.iter()
            .find(|s| s.name == server_name)
            .ok_or_else(|| anyhow::anyhow!("MCP 服务器不存在: {}", server_name))?
            .clone();

        // 连接 → 拉取 → 缓存 → 断开
        let (name, tools) = self.config_manager.fetch_and_cache_tools(&server_entry).await?;

        // 1. 更新 ToolRegistry（卸载旧 → 注册新）
        let _ = tool_registry.unregister_mcp_server_tools(&name);
        let categories = parse_categories(&server_entry.categories);
        let count = register_tools_to_registry(tool_registry, &name, &tools, &categories);

        // 2. 同步到 McpManager 内部 ToolManager（确保 call_tool_auto 路由正确）
        self.manager.set_server_tools(&name, tools);

        tracing::info!(
            "刷新完成: server '{}' → {} 个工具（ToolRegistry + McpManager 已同步）",
            name, count
        );
        Ok((name, count))
    }
}
