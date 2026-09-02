use async_trait::async_trait;
use anyhow::Result;
use serde_json::Value;
use std::sync::Arc;
use planned_agent_core::mcp::types::{Tool, ToolResult};
// McpManagerTrait 已下沉到 core（依赖反转）。
// 这里只重新导出，保持现有 `use planned_agent_tool_manager::McpManagerTrait` 的调用方式可用。
pub use planned_agent_core::tool_registry::traits::McpManagerTrait;

/// MCP 管理器适配器
///
/// 包装实现了 `McpManagerTrait` 的对象，提供统一接口；
/// 一般用于在 `ToolRegistry` 之外再做一层装饰（例如多路复用、缓存等）。
pub struct McpManagerAdapter {
    inner: Arc<dyn McpManagerTrait>,
}

impl McpManagerAdapter {
    /// 创建新的适配器
    pub fn new(manager: Arc<dyn McpManagerTrait>) -> Self {
        Self { inner: manager }
    }

    /// 获取内部管理器的引用
    pub fn inner(&self) -> &Arc<dyn McpManagerTrait> {
        &self.inner
    }
}

#[async_trait]
impl McpManagerTrait for McpManagerAdapter {
    async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        self.inner.call_tool(tool_name, arguments).await
    }

    fn get_all_tools(&self) -> Vec<Tool> {
        self.inner.get_all_tools()
    }

    fn find_server_for_tool(&self, tool_name: &str) -> Option<String> {
        self.inner.find_server_for_tool(tool_name)
    }

    fn get_server_tools(&self, server_name: &str) -> Vec<Tool> {
        self.inner.get_server_tools(server_name)
    }

    fn get_server_names(&self) -> Vec<String> {
        self.inner.get_server_names()
    }

    fn get_server_categories(&self, server_name: &str) -> Option<Vec<String>> {
        self.inner.get_server_categories(server_name)
    }
}