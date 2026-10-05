use std::sync::Arc;
use serde_json::Value;
use anyhow::Result;
use crate::mcp::types::{Tool, ToolResult};
use super::types::ToolCategory;

/// 工具执行器 trait（抽象层）
/// 
/// 定义在 core 中，由 tool-manager 实现
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// 执行工具调用
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult>;
    
    /// 获取执行器名称
    fn name(&self) -> &str;
    
    /// 获取执行器描述
    fn description(&self) -> &str {
        ""
    }
    
    /// 获取此执行器支持的工具名称列表
    fn supported_tools(&self) -> Vec<String>;
    
    /// 检查是否支持指定工具
    fn supports_tool(&self, tool_name: &str) -> bool {
        self.supported_tools().contains(&tool_name.to_string())
    }
}

/// 内置工具提供者 trait（抽象层）
/// 
/// 定义在 core 中，由 tool-manager 实现
pub trait BuiltinToolProvider: Send + Sync {
    /// 获取提供的工具列表
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)>;

    /// 获取执行器（Arc 版本，便于共享）
    fn executor(&self) -> Arc<dyn ToolExecutor>;
}

/// MCP 管理器 trait（抽象层）
///
/// 定义在 core 中，由具体的 MCP 实现（如 mcp-rmcp）实现，
/// 然后以 `Arc<dyn McpManagerTrait>` 注入到 `ToolRegistry`。
///
/// 这样 `ToolRegistry` 与具体 MCP 实现解耦：
///   - `tool-manager` 不必反向依赖 `mcp-rmcp`
///   - `mcp-rmcp` 只需要实现这个 trait，就能被任意 `ToolRegistry` 接受
#[async_trait::async_trait]
pub trait McpManagerTrait: Send + Sync {
    /// 调用工具（按工具名自动路由到对应服务器）
    async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<ToolResult>;

    /// 获取所有已注册工具
    fn get_all_tools(&self) -> Vec<Tool>;

    /// 获取指定服务器已登记的工具列表（运行时登记表，不触发连接）
    fn get_server_tools(&self, server_name: &str) -> Vec<Tool>;

    /// 查找工具所在的服务器
    fn find_server_for_tool(&self, tool_name: &str) -> Option<String>;

    /// 获取所有已连接的服务器名称
    fn get_server_names(&self) -> Vec<String>;

    /// 获取服务器配置的分类（用于工具元数据）
    fn get_server_categories(&self, server_name: &str) -> Option<Vec<String>>;
}

/// 统一工具注册表 trait（抽象层）
///
/// 与 [`ToolExecutor`] 的分工：
/// - [`ToolExecutor`] 是**单个**执行器，只认自己 `supported_tools()` 里的那几个工具；
/// - 本 trait 是**统一入口**，按工具名把调用路由到 MCP / 自定义 / 内置里的任意一方。
///
/// 为什么要这层抽象：统一入口此前只存在于 `tool-manager` 的 `ToolRegistry` **具体类型**上，
/// 下游想用它就得依赖整个 `tool-manager`。抽象提到这里之后，只需要「按名字调工具」
/// 能力的下游（如 `script-lua`）**只依赖 `core`** 即可。
///
/// 与 [`McpManagerTrait`] 同构：抽象在 core，实现在上层。
#[async_trait::async_trait]
pub trait ToolRegistryTrait: Send + Sync {
    /// 按工具名调用（内部路由到 MCP / 自定义 / 内置）。
    async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<ToolResult>;

    /// 列出全部可用工具名，供调用方（脚本 / UI）枚举。
    fn tool_names(&self) -> Vec<String>;
}
