//! Errors 模块
//!
//! Agent 错误类型定义。

pub mod error_types;

// 注意：内部错误类型不导出到 core 顶层
// 如需使用，通过完整路径访问：
//   planned_agent_core::errors::error_types::AgentError
