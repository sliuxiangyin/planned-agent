//! Tool Registry 模块
//!
//! 工具注册与执行抽象。
//!
//! 本模块定义 `ToolRegistry` 需要的全部抽象类型（`ToolSource`、`ToolCategory`、
//! `ToolExecutor`、`BuiltinToolProvider`、`McpManagerTrait`），
//! 具体实现在下游 crate（`tool-manager`、`mcp-rmcp` 等）中完成。
//!
//! 这种"抽象在 core，实现在上层"的模式避免了 crate 之间相互依赖。

pub mod types;
pub mod traits;

// 在模块顶层重新导出常用类型，避免下游写完整路径
pub use types::{ToolCategory, ToolSource};
pub use traits::{BuiltinToolProvider, McpManagerTrait, ToolExecutor};