//! Prompt 模块
//!
//! 提供 Prompt 管理抽象。

pub mod traits;

// 导出公共 trait 和类型
pub use traits::{
    PromptManager, PromptTemplate, PromptContext, PromptInfo,
    PromptMetadata, PromptVariable, OutputSchema, OutputFormat,
};
