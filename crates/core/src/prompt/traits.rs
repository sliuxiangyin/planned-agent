use async_trait::async_trait;
use anyhow::Result;
use serde_json::Value;
use super::{PromptTemplate, PromptContext, PromptInfo};

/// Prompt管理器 trait
#[async_trait]
pub trait PromptManager: Send + Sync {
    /// 加载prompt模板
    async fn load_template(&self, name: &str) -> Result<PromptTemplate>;
    
    /// 渲染prompt模板
    async fn render(&self, name: &str, context: &PromptContext) -> Result<String>;
    
    /// 列出所有可用prompt
    async fn list_prompts(&self) -> Result<Vec<PromptInfo>>;
    
    /// 检查prompt是否存在
    async fn exists(&self, name: &str) -> Result<bool>;
    
    /// 重新加载所有prompt（用于热更新）
    async fn reload(&self) -> Result<()>;
    
    /// 获取prompt的输出schema（如果定义了）
    async fn get_output_schema(&self, name: &str) -> Result<Option<Value>>;
    
    /// 将LLM结果转换为结构化类型
    async fn parse_response<T: serde::de::DeserializeOwned>(
        &self, 
        name: &str, 
        response: &str
    ) -> Result<T>;
    
    /// 验证LLM结果是否符合schema
    async fn validate_response(&self, name: &str, response: &str) -> Result<bool>;
}
