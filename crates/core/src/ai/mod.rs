//! AI 模块
//!
//! 提供 AI 交互抽象，支持多种 LLM 提供商。

pub mod config;
pub mod traits;
pub mod types;

// 导出公共 trait
pub use traits::{AiClient, ChatCompletionStream};
