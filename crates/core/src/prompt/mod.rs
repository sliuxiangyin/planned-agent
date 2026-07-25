pub mod traits;

pub use traits::PromptManager;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 输出格式枚举
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Json,
    Text,
    Markdown,
    Yaml,
    Xml,
}

/// 输出格式定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSchema {
    /// 输出类型：json, text, markdown等
    pub format: OutputFormat,
    /// JSON Schema（当format为json时）
    pub json_schema: Option<Value>,
    /// 示例输出
    pub example: Option<String>,
    /// 输出约束说明（会添加到prompt中）
    pub constraints: Option<String>,
}

/// Prompt模板
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplate {
    pub name: String,
    pub content: String,
    pub metadata: PromptMetadata,
    pub output_schema: Option<OutputSchema>,
}

/// Prompt元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptMetadata {
    pub description: Option<String>,
    pub version: Option<String>,
    pub variables: Vec<PromptVariable>,
    pub tags: Vec<String>,
}

/// Prompt变量定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptVariable {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
    pub default_value: Option<Value>,
}

/// Prompt上下文（用于渲染）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptContext {
    pub variables: HashMap<String, Value>,
    pub metadata: HashMap<String, Value>,
}

/// Prompt信息（用于列表）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptInfo {
    pub name: String,
    pub description: Option<String>,
    pub variables: Vec<String>,
    pub has_output_schema: bool,
}

impl PromptContext {
    /// 创建新的空上下文
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
            metadata: HashMap::new(),
        }
    }
    
    /// 添加变量
    pub fn with_variable(mut self, key: impl Into<String>, value: Value) -> Self {
        self.variables.insert(key.into(), value);
        self
    }
    
    /// 添加元数据
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
}

impl Default for PromptContext {
    fn default() -> Self {
        Self::new()
    }
}
