//! MCP 模块
//!
//! Model Context Protocol 实现。

pub mod traits;

// 注意：内部 trait 不导出到 core 顶层
// 如需使用，通过完整路径访问：
//   planned_agent_core::mcp::traits::McpClient
