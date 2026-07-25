pub mod types;
pub mod executor;
pub mod registry;
pub mod custom_tool;
pub mod mcp_adapter;
pub mod validator;
pub mod builtin;

// 重新导出主要类型（来自 core）
pub use planned_agent_core::tool_registry::{
    ToolSource, 
    ToolCategory, 
    ToolExecutor, 
    BuiltinToolProvider,
};

// 导出本 crate 的类型
pub use types::{ToolMetadata, ToolRegistryStats, ToolOutcome};
pub use registry::ToolRegistry;
pub use custom_tool::CustomToolExecutor;
pub use mcp_adapter::{McpManagerTrait, McpManagerAdapter};
