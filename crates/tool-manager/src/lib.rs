// 核心模块
pub mod core;
// 子 Agent 模块
pub mod sub_agent;
// 适配器模块
pub mod adapter;
// 内置工具模块
pub mod builtin;

// 向后兼容：保留旧的 `types` 路径
pub mod types {
    pub use crate::core::types::*;
}

// 重新导出主要类型（来自 core）
pub use planned_agent_core::tool_registry::{
    ToolSource,
    ToolCategory,
    ToolExecutor,
    BuiltinToolProvider,
};
// McpManagerTrait 已下沉到 core；为兼容旧调用方式继续从本 crate 重新导出
pub use planned_agent_core::tool_registry::traits::McpManagerTrait;

// 重新导出本 crate 的类型
pub use core::{ToolMetadata, ToolRegistryStats, ToolOutcome};
pub use core::registry::ToolRegistry;
pub use adapter::custom::CustomToolExecutor;
pub use adapter::mcp::McpManagerAdapter;
pub use sub_agent::stream::{StreamKind, ToolStreamEvent, ToolStreamSender};
pub use sub_agent::executor::{
    OneShotSubAgentRunner, SubAgentRunOutcome, SubAgentSession, SubAgentSessionRunner,
    SubAgentToolExecutor,
};
pub use sub_agent::session::SubAgentSessionStore;