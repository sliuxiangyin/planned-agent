use async_trait::async_trait;
use anyhow::Result;
use async_openai::{Client, config::OpenAIConfig};
use planned_agent_core::{
    ai::{AiClient, ChatCompletionStream},
    types::{
        ChatCompletionRequest, ChatCompletionResponse, ChatCompletionChunk,
        Message, MessageRole, MessageContent, ToolCall, ToolType, FunctionCall,
        Choice, FinishReason, Usage, ChunkChoice, DeltaMessage, DeltaToolCall, DeltaFunctionCall,
        ToolDefinition, Conversation, ThinkingConfig,
    },
};
use futures::StreamExt;
use tracing::{info, warn, error};
use std::collections::HashMap;

/// OpenAI 客户端配置
pub struct OpenAiClientConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: Option<String>,
    pub default_temperature: Option<f32>,
    pub default_max_tokens: Option<u32>,
    pub organization: Option<String>,
    /// 思考模式配置（适用于支持思考模式的AI模型）
    pub thinking_config: Option<ThinkingConfig>,
}

/// OpenAI 客户端实现
pub struct OpenAiClient {
    client: Client<OpenAIConfig>,
    config: OpenAiClientConfig,
}

impl OpenAiClient {
    /// 创建新的 OpenAI 客户端
    pub fn new(config: OpenAiClientConfig) -> Self {
        let mut openai_config = OpenAIConfig::new()
            .with_api_key(&config.api_key);
        
        // 设置自定义 base_url（用于 DeepSeek 等兼容 API）
        if let Some(base_url) = &config.base_url {
            info!("Setting custom API base URL: {}", base_url);
            openai_config = openai_config.with_api_base(base_url);
        } else {
            info!("Using default OpenAI API URL");
        }
        
        // 设置组织 ID
        if let Some(org_id) = &config.organization {
            openai_config = openai_config.with_org_id(org_id);
        }
        
        let client = Client::with_config(openai_config);
        
        Self { client, config }
    }
    
    /// 从会话创建客户端
    pub fn from_conversation(conversation: &Conversation, api_key: String) -> Self {
        let config = OpenAiClientConfig {
            api_key,
            model: conversation.model.clone(),
            base_url: None,
            default_temperature: conversation.temperature,
            default_max_tokens: conversation.max_tokens,
            organization: None,
            thinking_config: None,
        };
        Self::new(config)
    }
    
    /// 转换消息到 OpenAI 格式
    fn convert_message(&self, message: &Message) -> Result<async_openai::types::ChatCompletionRequestMessage> {
        match message.role {
            MessageRole::System => {
                let content = match &message.content {
                    Some(MessageContent::Text { text }) => text.clone(),
                    _ => return Err(anyhow::anyhow!("System message must have text content")),
                };
                Ok(async_openai::types::ChatCompletionRequestMessage::System(
                    async_openai::types::ChatCompletionRequestSystemMessage {
                        content: async_openai::types::ChatCompletionRequestSystemMessageContent::Text(content),
                        name: message.name.clone(),
                    }
                ))
            }
            MessageRole::User => {
                let content = match &message.content {
                    Some(MessageContent::Text { text }) => {
                        async_openai::types::ChatCompletionRequestUserMessageContent::Text(text.clone())
                    }
                    Some(MessageContent::Image { image_url }) => {
                        // 图片内容暂时作为文本处理
                        let image_text = format!("Image: {}", image_url.url);
                        async_openai::types::ChatCompletionRequestUserMessageContent::Text(image_text)
                    }
                    _ => return Err(anyhow::anyhow!("User message must have text or image content")),
                };
                Ok(async_openai::types::ChatCompletionRequestMessage::User(
                    async_openai::types::ChatCompletionRequestUserMessage {
                        content,
                        name: message.name.clone(),
                    }
                ))
            }
            MessageRole::Assistant => {
                let content = match &message.content {
                    Some(MessageContent::Text { text }) => Some(async_openai::types::ChatCompletionRequestAssistantMessageContent::Text(text.clone())),
                    None => None,
                    _ => return Err(anyhow::anyhow!("Assistant message must have text content or no content")),
                };
                let tool_calls = message.tool_calls.as_ref().map(|calls| {
                    calls.iter().map(|call| {
                        async_openai::types::ChatCompletionMessageToolCall {
                            id: call.id.clone(),
                            r#type: async_openai::types::ChatCompletionToolType::Function,
                            function: async_openai::types::FunctionCall {
                                name: call.function.name.clone(),
                                arguments: call.function.arguments.clone(),
                            },
                        }
                    }).collect()
                });
                Ok(async_openai::types::ChatCompletionRequestMessage::Assistant(
                    async_openai::types::ChatCompletionRequestAssistantMessage {
                        content,
                        tool_calls,
                        name: message.name.clone(),
                        ..Default::default()
                    }
                ))
            }
            MessageRole::Tool => {
                let tool_call_id = message.tool_call_id.clone()
                    .ok_or_else(|| anyhow::anyhow!("Tool message must have tool_call_id"))?;
                let content = match &message.content {
                    Some(MessageContent::ToolResult { content, .. }) => async_openai::types::ChatCompletionRequestToolMessageContent::Text(content.clone()),
                    Some(MessageContent::Text { text }) => async_openai::types::ChatCompletionRequestToolMessageContent::Text(text.clone()),
                    _ => return Err(anyhow::anyhow!("Tool message must have text or tool_result content")),
                };
                Ok(async_openai::types::ChatCompletionRequestMessage::Tool(
                    async_openai::types::ChatCompletionRequestToolMessage {
                        tool_call_id,
                        content,
                    }
                ))
            }
        }
    }
    
    /// 转换工具定义
    fn convert_tool(&self, tool: &ToolDefinition) -> async_openai::types::ChatCompletionTool {
        async_openai::types::ChatCompletionTool {
            r#type: async_openai::types::ChatCompletionToolType::Function,
            function: async_openai::types::FunctionObject {
                name: tool.function.name.clone(),
                description: tool.function.description.clone(),
                parameters: tool.function.parameters.clone(),
                strict: tool.function.strict,
            },
        }
    }
    
    /// 转换请求
    fn convert_request(&self, request: &ChatCompletionRequest) -> Result<async_openai::types::CreateChatCompletionRequest> {
        let messages: Vec<async_openai::types::ChatCompletionRequestMessage> = request.messages
            .iter()
            .map(|msg| self.convert_message(msg))
            .collect::<Result<Vec<_>>>()?;
        
        let tools: Option<Vec<async_openai::types::ChatCompletionTool>> = request.tools.as_ref().map(|tools| {
            tools.iter().map(|tool| self.convert_tool(tool)).collect()
        });
        
        let mut builder = async_openai::types::CreateChatCompletionRequestArgs::default();
        builder.model(&request.model);
        builder.messages(messages);
        
        if let Some(tools) = tools {
            builder.tools(tools);
        }
        
        if let Some(temperature) = request.temperature {
            builder.temperature(temperature);
        } else if let Some(temperature) = self.config.default_temperature {
            builder.temperature(temperature);
        }
        
        if let Some(max_tokens) = request.max_tokens {
            builder.max_tokens(max_tokens);
        } else if let Some(max_tokens) = self.config.default_max_tokens {
            builder.max_tokens(max_tokens);
        }
        
        builder.stream(request.stream);
        
        let mut chat_request = builder.build()?;
        
        // 设置思考模式参数
        if let Some(thinking_config) = &self.config.thinking_config {
            if thinking_config.enabled {
                // 设置思考模式开关
                let thinking = serde_json::json!({
                    "type": "enabled"
                });
                
                // 使用 metadata 字段传递 thinking 参数
                chat_request.metadata = Some(serde_json::json!({
                    "thinking": thinking
                }));
                
                // 设置思考强度
                let effort = thinking_config.effort.as_deref().unwrap_or("high");
                let reasoning_effort = match effort {
                    "high" => async_openai::types::ReasoningEffort::High,
                    "medium" => async_openai::types::ReasoningEffort::Medium,
                    "low" => async_openai::types::ReasoningEffort::Low,
                    _ => async_openai::types::ReasoningEffort::High,
                };
                chat_request.reasoning_effort = Some(reasoning_effort);
            }
        }
        
        // 设置额外参数
        for (key, value) in &request.extra {
            match key.as_str() {
                "top_p" => {
                    if let Some(top_p) = value.as_f64() {
                        chat_request.top_p = Some(top_p as f32);
                    }
                }
                "frequency_penalty" => {
                    if let Some(penalty) = value.as_f64() {
                        chat_request.frequency_penalty = Some(penalty as f32);
                    }
                }
                "presence_penalty" => {
                    if let Some(penalty) = value.as_f64() {
                        chat_request.presence_penalty = Some(penalty as f32);
                    }
                }
                "stop" => {
                    if let Ok(stop) = serde_json::from_value::<Vec<String>>(value.clone()) {
                        chat_request.stop = Some(async_openai::types::Stop::StringArray(stop));
                    }
                }
                _ => {}
            }
        }
        
        Ok(chat_request)
    }
    
    /// 转换响应
    fn convert_response(&self, response: async_openai::types::CreateChatCompletionResponse) -> Result<ChatCompletionResponse> {
        let choices = response.choices.iter().map(|choice| {
            let message = Message {
                role: MessageRole::Assistant,
                content: choice.message.content.as_ref().map(|c| MessageContent::Text {
                    text: c.clone()
                }),
                tool_calls: choice.message.tool_calls.as_ref().map(|calls| {
                    calls.iter().map(|call| ToolCall {
                        id: call.id.clone(),
                        r#type: ToolType::Function,
                        function: FunctionCall {
                            name: call.function.name.clone(),
                            arguments: call.function.arguments.clone(),
                        },
                    }).collect()
                }),
                tool_call_id: None,
                name: None,
                reasoning_content: None, // async-openai 库不支持此字段
            };
            
            Choice {
                index: choice.index as u32,
                message,
                finish_reason: choice.finish_reason.map(|r| match r {
                    async_openai::types::FinishReason::Stop => FinishReason::Stop,
                    async_openai::types::FinishReason::Length => FinishReason::Length,
                    async_openai::types::FinishReason::ToolCalls => FinishReason::ToolCalls,
                    async_openai::types::FinishReason::ContentFilter => FinishReason::ContentFilter,
                    async_openai::types::FinishReason::FunctionCall => FinishReason::FunctionCall,
                }),
                logprobs: None,
            }
        }).collect();
        
        Ok(ChatCompletionResponse {
            id: response.id,
            object: response.object,
            created: response.created as u64,
            model: response.model,
            choices,
            usage: response.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
            system_fingerprint: response.system_fingerprint,
        })
    }
    
    /// 转换流式响应块
    fn convert_chunk(&self, chunk: async_openai::types::CreateChatCompletionStreamResponse) -> Result<ChatCompletionChunk> {
        let choices = chunk.choices.iter().map(|choice| {
            let delta = DeltaMessage {
                role: choice.delta.role.as_ref().map(|r| match r {
                    async_openai::types::Role::System => MessageRole::System,
                    async_openai::types::Role::User => MessageRole::User,
                    async_openai::types::Role::Assistant => MessageRole::Assistant,
                    async_openai::types::Role::Tool => MessageRole::Tool,
                    async_openai::types::Role::Function => MessageRole::Assistant,
                }),
                content: choice.delta.content.clone(),
                tool_calls: choice.delta.tool_calls.as_ref().map(|calls| {
                    calls.iter().map(|call| DeltaToolCall {
                        index: call.index as u32,
                        id: call.id.clone(),
                        r#type: call.r#type.as_ref().map(|t| match t {
                            async_openai::types::ChatCompletionToolType::Function => ToolType::Function,
                        }),
                        function: call.function.as_ref().map(|f| DeltaFunctionCall {
                            name: f.name.clone(),
                            arguments: f.arguments.clone(),
                        }),
                    }).collect()
                }),
                reasoning_content: None, // async-openai 库不支持此字段，需要从 extra 字段中提取
            };
            
            ChunkChoice {
                index: choice.index as u32,
                delta,
                finish_reason: choice.finish_reason.map(|r| match r {
                    async_openai::types::FinishReason::Stop => FinishReason::Stop,
                    async_openai::types::FinishReason::Length => FinishReason::Length,
                    async_openai::types::FinishReason::ToolCalls => FinishReason::ToolCalls,
                    async_openai::types::FinishReason::ContentFilter => FinishReason::ContentFilter,
                    async_openai::types::FinishReason::FunctionCall => FinishReason::FunctionCall,
                }),
                logprobs: None,
            }
        }).collect();
        
        Ok(ChatCompletionChunk {
            id: chunk.id,
            object: chunk.object,
            created: chunk.created as u64,
            model: chunk.model,
            choices,
            system_fingerprint: chunk.system_fingerprint,
            usage: chunk.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        })
    }
}

#[async_trait]
impl AiClient for OpenAiClient {
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        info!("Sending request to OpenAI API");
        
        let chat_request = self.convert_request(&request)?;
        
        // 添加重试逻辑
        let mut retries = 0;
        let max_retries = 3;
        
        loop {
            match self.client.chat().create(chat_request.clone()).await {
                Ok(response) => {
                    info!("Received response from OpenAI API");
                    return self.convert_response(response);
                }
                Err(e) => {
                    retries += 1;
                    if retries >= max_retries {
                        error!("OpenAI API request failed after {} retries: {}", retries, e);
                        return Err(e.into());
                    }
                    warn!("OpenAI API request failed, retrying ({}/{}): {}", retries, max_retries, e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
    }
    
    async fn chat_completion_stream(&self, request: ChatCompletionRequest) -> Result<ChatCompletionStream> {
        info!("Sending streaming request to OpenAI API");
        
        let mut stream_request = request;
        stream_request.stream = true;
        
        let chat_request = self.convert_request(&stream_request)?;
        
        let stream = self.client.chat().create_stream(chat_request).await?;
        
        let client = std::sync::Arc::new(self.clone());
        
        let mapped_stream = stream.map(move |chunk| {
            match chunk {
                Ok(chunk) => {
                    client.convert_chunk(chunk)
                }
                Err(e) => Err(e.into()),
            }
        });
        
        let boxed_stream: Box<dyn futures::Stream<Item = Result<ChatCompletionChunk>> + Send + Unpin> = 
            Box::new(mapped_stream);
        
        Ok(ChatCompletionStream::new(boxed_stream))
    }
    
    fn provider_name(&self) -> &str {
        "openai"
    }
    
    fn model_name(&self) -> &str {
        &self.config.model
    }
    
    fn default_config(&self) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: self.config.model.clone(),
            messages: Vec::new(),
            tools: None,
            temperature: self.config.default_temperature,
            max_tokens: self.config.default_max_tokens,
            stream: false,
            extra: HashMap::new(),
        }
    }
}

impl Clone for OpenAiClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            config: OpenAiClientConfig {
                api_key: self.config.api_key.clone(),
                model: self.config.model.clone(),
                base_url: self.config.base_url.clone(),
                default_temperature: self.config.default_temperature,
                default_max_tokens: self.config.default_max_tokens,
                organization: self.config.organization.clone(),
                thinking_config: self.config.thinking_config.clone(),
            },
        }
    }
}
