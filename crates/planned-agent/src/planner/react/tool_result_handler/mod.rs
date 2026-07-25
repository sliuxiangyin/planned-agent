//! 工具结果 handler 集合
//!
//! 本目录文件实现 [`crate::planner::react::tool_result_router::ObservationPostHandler`] 契约。
//! 每个 handler 自己管自己的依赖（构造时注入）—— router 完全无感。
//!
//! 注册方式：调用方在 [`crate::planner::react::tool_result_router::ToolResultRouter`] 上
//! 调 `register(kind, handler)`，本目录不参与注册聚合，保持轻量。

pub mod binary_truncate;
pub mod html_clean;

pub use binary_truncate::BinaryTruncatePostHandler;
pub use html_clean::HtmlBrowserPostHandler;
