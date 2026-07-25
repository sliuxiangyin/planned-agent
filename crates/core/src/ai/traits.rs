use async_trait::async_trait;
use anyhow::Result;
use crate::types::{ChatCompletionRequest, ChatCompletionResponse, ChatCompletionChunk};

/// AI 客户端 trait（符合业内规范）
#[async_trait]
pub trait AiClient: Send + Sync {
    /// 发送聊天完成请求
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse>;
    
    /// 发送流式聊天完成请求
    async fn chat_completion_stream(&self, request: ChatCompletionRequest) -> Result<ChatCompletionStream>;
    
    /// 获取提供商名称
    fn provider_name(&self) -> &str;
    
    /// 获取模型名称
    fn model_name(&self) -> &str;
    
    /// 获取默认配置
    fn default_config(&self) -> ChatCompletionRequest;
}

/// 流式响应包装器
pub struct ChatCompletionStream {
    pub stream: Box<dyn futures::Stream<Item = Result<ChatCompletionChunk>> + Send + Unpin>,
}

impl ChatCompletionStream {
    pub fn new(stream: Box<dyn futures::Stream<Item = Result<ChatCompletionChunk>> + Send + Unpin>) -> Self {
        Self { stream }
    }
}
