//! 工具注册表 GUI 适配层
//!
//! 设计要点：
//! - `init()` **不依赖** McpContext，只注册 6 个内置 provider
//! - McpManager 由 `app()` 在 MCP 就绪后通过 `set_mcp_manager()` 延后注入
//! - ToolRegistry 内部已用 `RwLock<Option<...>>`，天然支持延后设置与将来替换
//! - **不新增任何占位/扩展 API**——按需再设计

use std::sync::Arc;

use planned_agent_mcp_rmcp::McpManager;
use planned_agent_tool_manager::builtin::{
    ai_tools::AiToolsProvider, data_tools::DataToolsProvider, file_tools::FileToolsProvider,
    system_tools::SystemToolsProvider, text_tools::TextToolsProvider,
    web_tools::WebToolsProvider,
};
use planned_agent_tool_manager::ToolRegistry;

/// GUI 层 Tools 上下文
///
/// 组件通过 `use_context::<Resource<Option<Arc<ToolsContext>>>>()` 获取，
/// 再通过 `ctx.registry.get_all_tools()` / `ctx.registry.call_tool(...)` 访问工具。
pub struct ToolsContext {
    pub registry: Arc<ToolRegistry>,
}

impl ToolsContext {
    /// 同步初始化：构造 ToolRegistry + 注册 6 个内置 provider
    ///
    /// 此时 `mcp_manager` 为 None；MCP 工具由后续 `set_mcp_manager` 触发注入。
    pub fn init() -> anyhow::Result<Self> {
        let registry = ToolRegistry::new();

        // 按 CLI 既有顺序注册内置 provider（顺序无功能影响，仅日志可读性）
        registry.register_builtin_provider(&FileToolsProvider);
        registry.register_builtin_provider(&TextToolsProvider);
        registry.register_builtin_provider(&SystemToolsProvider);
        registry.register_builtin_provider(&DataToolsProvider);
        registry.register_builtin_provider(&AiToolsProvider);
        registry.register_builtin_provider(&WebToolsProvider);

        let stats = registry.get_stats();
        tracing::info!(
            "ToolRegistry 初始化完成（仅内置）: {} builtin tools",
            stats.builtin_count
        );

        Ok(Self {
            registry: Arc::new(registry),
        })
    }

    /// 延后注入 McpManager：转发到 `ToolRegistry::set_mcp_manager`
    ///
    /// 调用时机：McpContext 异步初始化完成后由 `app()` 主动调用。
    /// 多次调用行为：以后一次为准（McpManagerTrait 整体替换）。
    pub fn set_mcp_manager(&self, mgr: Arc<McpManager>) {
        self.registry.set_mcp_manager(mgr);
        let stats = self.registry.get_stats();
        tracing::info!(
            "MCP 注入完成: 总计 {} 工具 (内置 {} / MCP {})",
            stats.total,
            stats.builtin_count,
            stats.mcp_count
        );
    }
}