//! Tool Registry 模块
//!
//! 工具注册与执行抽象。

pub mod types;
pub mod traits;

// 注意：内部类型不导出到 core 顶层
// 如需使用，通过完整路径访问：
//   planned_agent_core::tool_registry::types::ToolCategory
