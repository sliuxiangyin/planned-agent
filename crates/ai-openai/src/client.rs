use async_trait::async_trait;
use anyhow::Result;
use async_openai::{Client, config::OpenAIConfig};
use planned_agent_core::{
    ai::{
        AiClient, ChatCompletionStream,
        config::ThinkingConfig,
        types::{
            ChatCompletionRequest, ChatCompletionResponse, ChatCompletionChunk,
            Message, MessageRole, MessageContent, ToolCall, ToolType, FunctionCall,
            Choice, FinishReason, Usage, ChunkChoice, DeltaMessage, DeltaToolCall, DeltaFunctionCall,
            ToolDefinition, Conversation,
        },
    },
};
use futures::StreamExt;
use tracing::{info, warn, error};
use std::collections::HashMap;

/// 兼容层：自定义非流式响应类型。
///
/// async-openai 0.41 内置的 `CreateChatCompletionResponse.service_tier` 使用严格枚举
/// `ServiceTier`，无法反序列化 MiniMax 等兼容提供商返回的 `"standard"`。这里通过
/// BYOT（Bring Your Own Types）自定义响应类型并**故意省略 `service_tier` 字段**，
/// 让 serde 默认忽略该未知字段，从而兼容各家提供商。其余字段仍复用库类型。
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatResponse {
    id: String,
    #[serde(default)]
    created: u32,
    model: String,
    choices: Vec<CompatChatResponseChoice>,
    #[serde(default)]
    system_fingerprint: Option<String>,
    object: String,
    usage: Option<async_openai::types::chat::CompletionUsage>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatResponseChoice {
    index: u32,
    message: CompatChatResponseMessage,
    finish_reason: Option<async_openai::types::chat::FinishReason>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<CompatChatResponseToolCall>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatResponseToolCall {
    id: String,
    function: CompatFunctionCall,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatFunctionCall {
    name: String,
    arguments: String,
}

/// 兼容层：自定义流式 chunk 类型。同上，省略 `service_tier` 字段以兼容 MiniMax 等提供商。
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatStreamChunk {
    id: String,
    #[serde(default)]
    created: u32,
    model: String,
    choices: Vec<CompatChatStreamChoice>,
    #[serde(default)]
    system_fingerprint: Option<String>,
    object: String,
    usage: Option<async_openai::types::chat::CompletionUsage>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatStreamChoice {
    index: u32,
    delta: CompatChatStreamDelta,
    finish_reason: Option<async_openai::types::chat::FinishReason>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatStreamDelta {
    role: Option<async_openai::types::chat::Role>,
    content: Option<String>,
    tool_calls: Option<Vec<CompatChatStreamToolCall>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatStreamToolCall {
    index: u32,
    id: Option<String>,
    r#type: Option<async_openai::types::chat::FunctionType>,
    function: Option<async_openai::types::chat::FunctionCallStream>,
}

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
    fn convert_message(&self, message: &Message) -> Result<async_openai::types::chat::ChatCompletionRequestMessage> {
        match message.role {
            MessageRole::System => {
                let content = match &message.content {
                    Some(MessageContent::Text { text }) => text.clone(),
                    _ => return Err(anyhow::anyhow!("System message must have text content")),
                };
                Ok(async_openai::types::chat::ChatCompletionRequestMessage::System(
                    async_openai::types::chat::ChatCompletionRequestSystemMessage {
                        content: async_openai::types::chat::ChatCompletionRequestSystemMessageContent::Text(content),
                        name: message.name.clone(),
                    }
                ))
            }
            MessageRole::User => {
                let content = match &message.content {
                    Some(MessageContent::Text { text }) => {
                        async_openai::types::chat::ChatCompletionRequestUserMessageContent::Text(text.clone())
                    }
                    Some(MessageContent::Image { image_url }) => {
                        // 图片内容暂时作为文本处理
                        let image_text = format!("Image: {}", image_url.url);
                        async_openai::types::chat::ChatCompletionRequestUserMessageContent::Text(image_text)
                    }
                    _ => return Err(anyhow::anyhow!("User message must have text or image content")),
                };
                Ok(async_openai::types::chat::ChatCompletionRequestMessage::User(
                    async_openai::types::chat::ChatCompletionRequestUserMessage {
                        content,
                        name: message.name.clone(),
                    }
                ))
            }
            MessageRole::Assistant => {
                let content = match &message.content {
                    Some(MessageContent::Text { text }) => Some(async_openai::types::chat::ChatCompletionRequestAssistantMessageContent::Text(text.clone())),
                    None => None,
                    _ => return Err(anyhow::anyhow!("Assistant message must have text content or no content")),
                };
                let tool_calls = message.tool_calls.as_ref().map(|calls| {
                    calls.iter().map(|call| {
                        async_openai::types::chat::ChatCompletionMessageToolCalls::Function(
                            async_openai::types::chat::ChatCompletionMessageToolCall {
                                id: call.id.clone(),
                                function: async_openai::types::chat::FunctionCall {
                                    name: call.function.name.clone(),
                                    arguments: call.function.arguments.clone(),
                                },
                            }
                        )
                    }).collect()
                });
                Ok(async_openai::types::chat::ChatCompletionRequestMessage::Assistant(
                    async_openai::types::chat::ChatCompletionRequestAssistantMessage {
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
                    Some(MessageContent::ToolResult { content, .. }) => async_openai::types::chat::ChatCompletionRequestToolMessageContent::Text(content.clone()),
                    Some(MessageContent::Text { text }) => async_openai::types::chat::ChatCompletionRequestToolMessageContent::Text(text.clone()),
                    _ => return Err(anyhow::anyhow!("Tool message must have text or tool_result content")),
                };
                Ok(async_openai::types::chat::ChatCompletionRequestMessage::Tool(
                    async_openai::types::chat::ChatCompletionRequestToolMessage {
                        tool_call_id,
                        content,
                    }
                ))
            }
        }
    }
    
    /// 转换工具定义
    fn convert_tool(&self, tool: &ToolDefinition) -> async_openai::types::chat::ChatCompletionTool {
        async_openai::types::chat::ChatCompletionTool {
            function: async_openai::types::chat::FunctionObject {
                name: tool.function.name.clone(),
                description: tool.function.description.clone(),
                parameters: tool.function.parameters.clone(),
                strict: tool.function.strict,
            },
        }
    }
    
    /// 转换请求
    fn convert_request(&self, request: &ChatCompletionRequest) -> Result<async_openai::types::chat::CreateChatCompletionRequest> {
        let messages: Vec<async_openai::types::chat::ChatCompletionRequestMessage> = request.messages
            .iter()
            .map(|msg| self.convert_message(msg))
            .collect::<Result<Vec<_>>>()?;
        
        let tools: Option<Vec<async_openai::types::chat::ChatCompletionTools>> = request.tools.as_ref().map(|tools| {
            tools.iter().map(|tool| {
                async_openai::types::chat::ChatCompletionTools::Function(self.convert_tool(tool))
            }).collect()
        });
        
        let mut builder = async_openai::types::chat::CreateChatCompletionRequestArgs::default();
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
                }).into());
                
                // 设置思考强度
                let effort = thinking_config.effort.as_deref().unwrap_or("high");
                let reasoning_effort = match effort {
                    "high" => async_openai::types::chat::ReasoningEffort::High,
                    "medium" => async_openai::types::chat::ReasoningEffort::Medium,
                    "low" => async_openai::types::chat::ReasoningEffort::Low,
                    _ => async_openai::types::chat::ReasoningEffort::High,
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
                        chat_request.stop = Some(async_openai::types::chat::StopConfiguration::StringArray(stop));
                    }
                }
                _ => {}
            }
        }
        
        Ok(chat_request)
    }
    
    /// 转换响应
    fn convert_response(&self, response: CompatChatResponse) -> Result<ChatCompletionResponse> {
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
                    async_openai::types::chat::FinishReason::Stop => FinishReason::Stop,
                    async_openai::types::chat::FinishReason::Length => FinishReason::Length,
                    async_openai::types::chat::FinishReason::ToolCalls => FinishReason::ToolCalls,
                    async_openai::types::chat::FinishReason::ContentFilter => FinishReason::ContentFilter,
                    async_openai::types::chat::FinishReason::FunctionCall => FinishReason::FunctionCall,
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
    fn convert_chunk(&self, chunk: CompatChatStreamChunk) -> Result<ChatCompletionChunk> {
        let choices = chunk.choices.iter().map(|choice| {
            let delta = DeltaMessage {
                role: choice.delta.role.as_ref().map(|r| match r {
                    async_openai::types::chat::Role::System => MessageRole::System,
                    async_openai::types::chat::Role::User => MessageRole::User,
                    async_openai::types::chat::Role::Assistant => MessageRole::Assistant,
                    async_openai::types::chat::Role::Tool => MessageRole::Tool,
                    async_openai::types::chat::Role::Function => MessageRole::Assistant,
                }),
                content: choice.delta.content.clone(),
                tool_calls: choice.delta.tool_calls.as_ref().map(|calls| {
                    calls.iter().map(|call| DeltaToolCall {
                        index: call.index as u32,
                        id: call.id.clone(),
                        r#type: call.r#type.as_ref().map(|t| match t {
                            async_openai::types::chat::FunctionType::Function => ToolType::Function,
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
                    async_openai::types::chat::FinishReason::Stop => FinishReason::Stop,
                    async_openai::types::chat::FinishReason::Length => FinishReason::Length,
                    async_openai::types::chat::FinishReason::ToolCalls => FinishReason::ToolCalls,
                    async_openai::types::chat::FinishReason::ContentFilter => FinishReason::ContentFilter,
                    async_openai::types::chat::FinishReason::FunctionCall => FinishReason::FunctionCall,
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
        let req_json = serde_json::to_value(&chat_request)?;
        
        // 添加重试逻辑
        let mut retries = 0;
        let max_retries = 3;
        
        loop {
            match self.client.chat().create_byot::<serde_json::Value, CompatChatResponse>(req_json.clone()).await {
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
        let req_json = serde_json::to_value(&chat_request)?;
        
        let stream: async_openai::types::stream::StreamResponse<CompatChatStreamChunk> =
            self.client.chat().create_stream_byot(req_json).await?;
        
        let client = std::sync::Arc::new(self.clone());
        
        let mapped_stream = stream.map(move |chunk| {
            match chunk {
                Ok(chunk) => {
                    client.convert_chunk(chunk)
                }
                Err(e) => {
                    // 用 `{:?}` 保留 OpenAIError 变体信息（如 StreamError("...")），
                    // 比 Display 只显示 "stream failed: ..." 更有诊断价值。
                    Err(anyhow::anyhow!("OpenAI 流式错误: {:?}", e))
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 复现 MiniMax 兼容提供商流式响应中的 `service_tier: "standard"`，
    /// 验证自定义 chunk 类型能正常反序列化（未知字段被 serde 忽略）。
    #[test]
    fn stream_chunk_ignores_service_tier_standard() {
        let json = r#"{
            "id":"06e583330718211117f2b4c4eb2911b9",
            "choices":[{"index":0,"delta":{"content":"hello","role":"assistant"}}],
            "created":1788235827,
            "model":"MiniMax-M3",
            "object":"chat.completion.chunk",
            "usage":null,
            "service_tier":"standard"
        }"#;
        let chunk: CompatChatStreamChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.model, "MiniMax-M3");
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hello"));
        assert_eq!(chunk.choices[0].delta.role, Some(async_openai::types::chat::Role::Assistant));
    }

    /// 非流式响应同样可能携带 `service_tier`，验证自定义响应类型能忽略它。
    #[test]
    fn chat_response_ignores_service_tier_standard() {
        let json = r#"{
            "id":"chatcmpl-123",
            "object":"chat.completion",
            "created":1788235827,
            "model":"MiniMax-M3",
            "service_tier":"standard",
            "system_fingerprint":null,
            "choices":[{
                "index":0,
                "message":{"role":"assistant","content":"hello"},
                "finish_reason":"stop"
            }],
            "usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}
        }"#;
        let resp: CompatChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.model, "MiniMax-M3");
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("hello"));
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, 15);
    }

    /// DeepSeek 等推理模型在非流式 message 中携带 `reasoning_content`，
    /// content 可能为 null。自定义类型未声明该字段，serde 应忽略而非报错。
    #[test]
    fn deepseek_response_ignores_reasoning_content() {
        let json = r#"{
            "id":"chatcmpl-456",
            "object":"chat.completion",
            "created":1700000000,
            "model":"deepseek-chat",
            "choices":[{
                "index":0,
                "message":{
                    "role":"assistant",
                    "content":null,
                    "reasoning_content":"先拆解问题，再给出结论……"
                },
                "finish_reason":"stop"
            }],
            "usage":{"prompt_tokens":12,"completion_tokens":8,"total_tokens":20}
        }"#;
        let resp: CompatChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.model, "deepseek-chat");
        // content 为 null → None
        assert_eq!(resp.choices[0].message.content, None);
        // reasoning_content 被忽略，不影响反序列化
        assert_eq!(resp.choices[0].finish_reason, Some(async_openai::types::chat::FinishReason::Stop));
    }

    /// DeepSeek 流式 delta 同样带 `reasoning_content`，content 可为 null。
    #[test]
    fn deepseek_stream_chunk_ignores_reasoning_content() {
        let json = r#"{
            "id":"chatcmpl-456",
            "object":"chat.completion.chunk",
            "created":1700000000,
            "model":"deepseek-chat",
            "choices":[{
                "index":0,
                "delta":{"role":"assistant","content":null,"reasoning_content":"思考中……"},
                "finish_reason":null
            }]
        }"#;
        let chunk: CompatChatStreamChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.model, "deepseek-chat");
        assert_eq!(chunk.choices[0].delta.content, None);
        assert_eq!(chunk.choices[0].delta.role, Some(async_openai::types::chat::Role::Assistant));
    }
}

