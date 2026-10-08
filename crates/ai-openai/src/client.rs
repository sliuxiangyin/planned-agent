use async_trait::async_trait;
use anyhow::Result;
use async_openai::{Client, config::OpenAIConfig};
use planned_agent_core::{
    ai::{
        AiClient, ChatCompletionStream,
        config::ThinkingConfig,
        types::{
            ChatCompletionRequest, ChatCompletionResponse, ChatCompletionChunk,
            Message, MessageRole, MessageContent, ContentPart, ImageSource,
            ToolCall, ToolType, FunctionCall,
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
    #[serde(default, deserialize_with = "opt_finish_reason_or_none")]
    finish_reason: Option<async_openai::types::chat::FinishReason>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatResponseMessage {
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
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
    #[serde(default, deserialize_with = "opt_finish_reason_or_none")]
    finish_reason: Option<async_openai::types::chat::FinishReason>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatStreamDelta {
    role: Option<async_openai::types::chat::Role>,
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<CompatChatStreamToolCall>>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct CompatChatStreamToolCall {
    index: u32,
    #[serde(default)]
    id: Option<String>,
    /// MiniMax 等提供商会在 tool_calls 增量里给 `"type":""`；空串会令 `FunctionType`
    /// 枚举反序列化抛 `unknown variant ''` → 整块被丢弃 → 参数丢失。
    /// 宽容解析，空串/未知值视为 None（是否 function 由上层按 index/arguments 判断）。
    ///
    /// 必须带 `#[serde(default)]`：`deserialize_with` 会覆盖 serde 对缺失字段默认给的
    /// `Option = None` 语义——若不带 default，MiniMax 只发 `function.arguments`/`index`
    /// 而完全省略 `type`/`id` 字段时，会抛 `missing field type` → 含真实参数内容的分片
    /// 被丢弃 → 工具参数残缺/为空。default 保证字段缺失时取 None、存在时才走宽容解析。
    #[serde(default, deserialize_with = "opt_function_type_or_none")]
    r#type: Option<async_openai::types::chat::FunctionType>,
    #[serde(default)]
    function: Option<async_openai::types::chat::FunctionCallStream>,
}

/// 宽松解析 `finish_reason`：把空串或 `async_openai` 枚举不认识的任何非标值都当作 `None`，
/// 避免 MiniMax 等提供商返回 `finish_reason: ""` 时导致整块流/响应反序列化失败。
fn opt_finish_reason_or_none<'de, D>(
    de: D,
) -> Result<Option<async_openai::types::chat::FinishReason>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<serde_json::Value> as serde::Deserialize>::deserialize(de)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => {
            if v.as_str().is_some_and(|s| s.is_empty()) {
                return Ok(None);
            }
            // 未知非标值也宽容为 None，而不是让整块解析失败
            Ok(serde_json::from_value(v).ok())
        }
    }
}

/// 宽松解析流式 `tool_calls[].type`：把空串或非标值当作 `None`。
///
/// MiniMax 等提供商在 tool_calls 流式增量里会给 `"type":""`（甚至每个分片都重复带空串
/// `id`/`type`/`name` 占位）。若按枚举严格解析，空串会抛 `unknown variant ''`，导致
/// **整块（含真正 arguments 的分片）反序列化失败被丢弃** → 工具参数残缺/为空。
/// 此处宽容空串与未知值，保证含参数内容的分片得以保留、由上层按 index 累积。
fn opt_function_type_or_none<'de, D>(
    de: D,
) -> Result<Option<async_openai::types::chat::FunctionType>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<serde_json::Value> as serde::Deserialize>::deserialize(de)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => {
            if v.as_str().is_some_and(|s| s.is_empty()) {
                return Ok(None);
            }
            Ok(serde_json::from_value(v).ok())
        }
    }
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

/// 单张本地图片大小上限（与 OpenAI 单图上限一致：20 MB）。
/// 超限直接失败，避免把请求送到 API 才拿到 400。
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 本地图片扩展名 → MIME。白名单与 OpenAI vision 支持面一致（png / jpeg / webp / gif）。
fn image_mime_of(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// 大小校验（抽成纯函数，单测不必真造 20 MB 文件）。
fn check_image_size(len_bytes: u64, path: &std::path::Path) -> Result<()> {
    if len_bytes > MAX_IMAGE_BYTES {
        anyhow::bail!(
            "local image too large: {} is {} bytes (limit {} bytes / {} MB)",
            path.display(),
            len_bytes,
            MAX_IMAGE_BYTES,
            MAX_IMAGE_BYTES / 1024 / 1024
        );
    }
    Ok(())
}

/// 本地图片文件 → `data:{mime};base64,{...}`。
///
/// 失败一律返回 `Err`（不静默降级）：读不到图时静默丢图，只会让模型答得莫名其妙。
async fn local_image_to_data_url(path: &std::path::Path) -> Result<String> {
    let mime = image_mime_of(path).ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported local image extension: {} (supported: png, jpg, jpeg, webp, gif)",
            path.display()
        )
    })?;
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| anyhow::anyhow!("cannot read local image {}: {}", path.display(), e))?;
    check_image_size(meta.len(), path)?;
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| anyhow::anyhow!("cannot read local image {}: {}", path.display(), e))?;
    // 以**实际读到的**长度再校一次：元数据与正文之间文件可能被改写（TOCTOU）
    check_image_size(bytes.len() as u64, path)?;

    use base64::Engine as _;
    Ok(format!(
        "data:{};base64,{}",
        mime,
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

/// 把 `ImageSource::File` 就地解析成 `ImageSource::Url { url: data:… }`。
async fn resolve_image_source(source: &mut ImageSource) -> Result<()> {
    if let ImageSource::File { path, detail } = source {
        let url = local_image_to_data_url(path).await?;
        *source = ImageSource::Url {
            url,
            detail: detail.clone(),
        };
    }
    Ok(())
}

/// 发请求前把请求里的**本地图片**读盘 + base64 成 `data:` URL。
///
/// 只扫 User 消息 —— 底层（async-openai 0.41）只有 user content part 支持图片。
/// 在重试循环之外调用，因此一次请求只读盘一次。
async fn resolve_local_images(request: &mut ChatCompletionRequest) -> Result<()> {
    for (index, message) in request.messages.iter_mut().enumerate() {
        if !matches!(message.role, MessageRole::User) {
            continue;
        }
        match &mut message.content {
            Some(MessageContent::Image { image }) => {
                resolve_image_source(image)
                    .await
                    .map_err(|e| e.context(format!("message[{index}] (role=user)")))?;
            }
            Some(MessageContent::Parts { parts }) => {
                for (part_index, part) in parts.iter_mut().enumerate() {
                    if let ContentPart::Image { image } = part {
                        resolve_image_source(image).await.map_err(|e| {
                            e.context(format!("message[{index}].parts[{part_index}]"))
                        })?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// core 的图片消息段 → OpenAI 的 user content part。
fn convert_content_part(
    part: &ContentPart,
) -> Result<async_openai::types::chat::ChatCompletionRequestUserMessageContentPart> {
    use async_openai::types::chat::{
        ChatCompletionRequestMessageContentPartText,
        ChatCompletionRequestUserMessageContentPart,
    };
    match part {
        ContentPart::Text { text } => Ok(ChatCompletionRequestUserMessageContentPart::Text(
            ChatCompletionRequestMessageContentPartText { text: text.clone() },
        )),
        ContentPart::Image { image } => Ok(
            ChatCompletionRequestUserMessageContentPart::ImageUrl(convert_image_source(image)?),
        ),
    }
}

/// `ImageSource` → `{"type":"image_url","image_url":{"url":..,"detail":..}}`。
///
/// `File` 走到这里说明 `resolve_local_images` 没被调用（内部错误）—— 显式报错，
/// 而不是退回旧的 `"Image: {url}"` 文本降级。
fn convert_image_source(
    source: &ImageSource,
) -> Result<async_openai::types::chat::ChatCompletionRequestMessageContentPartImage> {
    use async_openai::types::chat::{
        ChatCompletionRequestMessageContentPartImage, ImageDetail as OaImageDetail, ImageUrl,
    };
    use planned_agent_core::ai::types::ImageDetail as CoreImageDetail;

    let (url, detail) = match source {
        ImageSource::Url { url, detail } => (url.clone(), detail.as_ref()),
        ImageSource::File { path, .. } => anyhow::bail!(
            "internal: local image {} was not resolved before conversion",
            path.display()
        ),
    };

    Ok(ChatCompletionRequestMessageContentPartImage {
        image_url: ImageUrl {
            url,
            detail: detail.map(|d| match d {
                CoreImageDetail::Low => OaImageDetail::Low,
                CoreImageDetail::High => OaImageDetail::High,
                CoreImageDetail::Auto => OaImageDetail::Auto,
            }),
        },
    })
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
                    // 图片只支持 user 角色（库的 system content part 只有 text）
                    _ => {
                        return Err(anyhow::anyhow!(
                            "System message must have text content (images are only supported on user messages)"
                        ))
                    }
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
                    Some(MessageContent::Image { image }) => {
                        async_openai::types::chat::ChatCompletionRequestUserMessageContent::Array(vec![
                            async_openai::types::chat::ChatCompletionRequestUserMessageContentPart::ImageUrl(
                                convert_image_source(image)?,
                            ),
                        ])
                    }
                    Some(MessageContent::Parts { parts }) => {
                        if parts.is_empty() {
                            return Err(anyhow::anyhow!(
                                "User message has empty content parts (nothing to send)"
                            ));
                        }
                        async_openai::types::chat::ChatCompletionRequestUserMessageContent::Array(
                            parts.iter().map(convert_content_part).collect::<Result<Vec<_>>>()?,
                        )
                    }
                    _ => return Err(anyhow::anyhow!("User message must have text, image or parts content")),
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
                    _ => return Err(anyhow::anyhow!("Assistant message must have text content or no content (images are only supported on user messages)")),
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
                    _ => return Err(anyhow::anyhow!("Tool message must have text or tool_result content (images are only supported on user messages)")),
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
        //
        // ⚠️ 这里**只**发标准的 `reasoning_effort`。以前还额外往 `metadata` 里塞了一个
        // 非标准的 `thinking:{type:"enabled"}`，MiniMax 这类严格校验的提供商会直接 400
        // （`invalid params, Mismatch type string with value object ... thinking ...`）。
        if let Some(thinking_config) = &self.config.thinking_config {
            if thinking_config.enabled {
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
            // 原生 reasoning_content 字段优先；缺失时从 content 中的 <think> 标签提取
            let native_reasoning = choice.message.reasoning_content.clone().unwrap_or_default();
            let raw_content = choice.message.content.as_deref().unwrap_or("");
            let (think_reasoning, clean) = if native_reasoning.is_empty() {
                split_think_content(raw_content, &mut false)
            } else {
                (String::new(), raw_content.to_string())
            };
            let reasoning = if native_reasoning.is_empty() {
                think_reasoning
            } else {
                native_reasoning
            };
            let content = if clean.is_empty() {
                None
            } else {
                Some(MessageContent::Text { text: clean })
            };
            let message = Message {
                role: MessageRole::Assistant,
                content,
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
                reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
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

        // 诊断：整条响应「既无正文、也无思考、也无工具调用」—— 这正是上层（flexible 单步）
        // 会判「空回答」的形态。此处**只留证据、不改行为**：`finish_reason` 是区分
        // 「provider 空响应」与「被 max_tokens 截断（`length`）」的唯一线索，而两者处置
        // 完全不同（前者重发即可，后者要动 `max_tokens`）。
        if is_empty_assistant_response(&response) {
            let raw = response
                .choices
                .iter()
                .map(|choice| {
                    let content = choice.message.content.as_deref().unwrap_or("");
                    let reasoning = choice.message.reasoning_content.as_deref().unwrap_or("");
                    let tool_calls = choice.message.tool_calls.as_ref().map(Vec::len).unwrap_or(0);
                    format!(
                        "index={} finish_reason={:?} content_chars={} \
                         reasoning_chars={} tool_calls={tool_calls}",
                        choice.index,
                        choice.finish_reason,
                        content.chars().count(),
                        reasoning.chars().count()
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ");
            tracing::debug!(
                model = %response.model,
                choices = response.choices.len(),
                completion_tokens = response
                    .usage
                    .as_ref()
                    .map(|usage| usage.completion_tokens)
                    .unwrap_or(0),
                prompt_tokens = response
                    .usage
                    .as_ref()
                    .map(|usage| usage.prompt_tokens)
                    .unwrap_or(0),
                "响应既无正文也无工具调用（空回答形态）—— 原始响应：{raw}"
            );
        }

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
    fn convert_chunk(&self, chunk: CompatChatStreamChunk, in_think: &mut bool) -> Result<ChatCompletionChunk> {
        let choices = chunk.choices.iter().map(|choice| {
            // 原生 reasoning_content 优先；缺失时从 content 中的 <think> 标签提取
            let native_reasoning = choice.delta.reasoning_content.clone().unwrap_or_default();
            let raw_content = choice.delta.content.as_deref().unwrap_or("");
            let (think_reasoning, clean) = if native_reasoning.is_empty() {
                split_think_content(raw_content, in_think)
            } else {
                (String::new(), raw_content.to_string())
            };
            let reasoning = if native_reasoning.is_empty() {
                think_reasoning
            } else {
                native_reasoning
            };
            let delta = DeltaMessage {
                role: choice.delta.role.as_ref().map(|r| match r {
                    async_openai::types::chat::Role::System => MessageRole::System,
                    async_openai::types::chat::Role::User => MessageRole::User,
                    async_openai::types::chat::Role::Assistant => MessageRole::Assistant,
                    async_openai::types::chat::Role::Tool => MessageRole::Tool,
                    async_openai::types::chat::Role::Function => MessageRole::Assistant,
                }),
                content: (!clean.is_empty()).then_some(clean),
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
                reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
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
    async fn chat_completion(&self, mut request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        info!("Sending request to OpenAI API");

        // 本地图片读盘 + base64（在重试循环之外，只做一次）
        resolve_local_images(&mut request).await?;
        
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

        // 本地图片读盘 + base64（在重试循环之外，只做一次）
        resolve_local_images(&mut stream_request).await?;
        
        let chat_request = self.convert_request(&stream_request)?;
        let req_json = serde_json::to_value(&chat_request)?;
        
        let stream: async_openai::types::stream::StreamResponse<CompatChatStreamChunk> =
            self.client.chat().create_stream_byot(req_json).await?;
        
        let client = std::sync::Arc::new(self.clone());
        
        // 流级 <think> 块状态：跨 chunk 记录是否已进入 think 且未闭合
        let mut in_think = false;
        let mapped_stream = stream.map(move |chunk| {
            match chunk {
                Ok(chunk) => {
                    client.convert_chunk(chunk, &mut in_think)
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

/// 一段可能缺省的文本是否「没有实质内容」（`None` 或全是空白）。
///
/// provider 返回 `""` / `"  \n"` 与 `null` 都应视为没有内容。
fn is_blank_text(text: &Option<String>) -> bool {
    text.as_deref().map_or(true, |text| text.trim().is_empty())
}

/// 整条响应是否「既无正文、也无思考、也无工具调用」。
///
/// 判定看**原始响应**字段（不看转换后的 `choices`）：转换会把空串的 `content` 折叠成
/// `None`，所以必须以原始值为准。`choices` 本身为空**不算**「空回答」—— 那是另一种
/// 异常（上层按「响应不含 choices」处理）。
///
/// 这条判据只用于**诊断日志**；真正决定「空回答」如何处置的是上层（`flexible` 单步的
/// 空回答重发）。两边口径必须一致，故抽成函数、可测。
fn is_empty_assistant_response(response: &CompatChatResponse) -> bool {
    !response.choices.is_empty()
        && response.choices.iter().all(|choice| {
            is_blank_text(&choice.message.content)
                && is_blank_text(&choice.message.reasoning_content)
                && choice
                    .message
                    .tool_calls
                    .as_ref()
                    .is_none_or(|calls| calls.is_empty())
        })
}

/// 把一段内容拆分为 `(reasoning, content)`。
///
/// 兼容部分兼容提供商把思考内容以 `<think>...</think>` 标签写在 `content`
/// （而非独立的 `reasoning_content` 字段）里的情况：标签内部内容提取为
/// `reasoning`（去掉标签），标签之外的内容作为 `content`。`in_think` 记录
/// 流式跨 chunk 的「已进入 think 块且未闭合」状态；非流式一次传入 `&mut false`。
fn split_think_content(seg: &str, in_think: &mut bool) -> (String, String) {
    const THINK_OPEN: &str = "<think>";
    const THINK_CLOSE: &str = "</think>";
    let mut reasoning = String::new();
    let mut content = String::new();
    if *in_think {
        // 已在 think 块内：等待闭合标签
        if let Some(pos) = seg.find(THINK_CLOSE) {
            let end = pos + THINK_CLOSE.len();
            reasoning.push_str(&seg[..pos]);
            content.push_str(&seg[end..]);
            *in_think = false;
        } else {
            reasoning.push_str(seg);
        }
    } else if let Some(start) = seg.find(THINK_OPEN) {
        content.push_str(&seg[..start]);
        let rest = &seg[start + THINK_OPEN.len()..];
        if let Some(pos) = rest.find(THINK_CLOSE) {
            let end = pos + THINK_CLOSE.len();
            reasoning.push_str(&rest[..pos]);
            content.push_str(&rest[end..]);
        } else {
            // think 未闭合，本 chunk 全部视为思考内容，等下一个 chunk
            reasoning.push_str(rest);
            *in_think = true;
        }
    } else {
        content.push_str(seg);
    }
    (reasoning, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空回答诊断的判据：`null` 与空串 / 纯空白都算「空」；有正文 / 有思考 / 有工具调用都不算。
    #[test]
    fn empty_response_detection_covers_null_blank_and_non_empty() {
        fn response(json: &str) -> CompatChatResponse {
            serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("测试 JSON 应能解析为 CompatChatResponse: {e}"))
        }

        // 无正文（null）→ 空
        assert!(is_empty_assistant_response(&response(
            r#"{"id":"1","object":"chat.completion","created":0,"model":"MiniMax-M3","usage":null,
                 "choices":[{"index":0,"message":{"role":"assistant","content":null},"finish_reason":"length"}]}"#
        )));
        // 空串 / 纯空白（provider 偶发）→ 同样算空
        assert!(is_empty_assistant_response(&response(
            r#"{"id":"1","object":"chat.completion","created":0,"model":"MiniMax-M3","usage":null,
                 "choices":[{"index":0,"message":{"role":"assistant","content":"  \n"},"finish_reason":"stop"}]}"#
        )));
        // 有正文 → 不算空
        assert!(!is_empty_assistant_response(&response(
            r#"{"id":"1","object":"chat.completion","created":0,"model":"MiniMax-M3","usage":null,
                 "choices":[{"index":0,"message":{"role":"assistant","content":"有内容"},"finish_reason":"stop"}]}"#
        )));
        // 只有思考内容 → 不算空（上层会把 reasoning 当产出）
        assert!(!is_empty_assistant_response(&response(
            r#"{"id":"1","object":"chat.completion","created":0,"model":"MiniMax-M3","usage":null,
                 "choices":[{"index":0,"message":{"role":"assistant","reasoning_content":"想了一下"},"finish_reason":"stop"}]}"#
        )));
        // 有工具调用 → 不算空
        assert!(!is_empty_assistant_response(&response(
            r#"{"id":"1","object":"chat.completion","created":0,"model":"MiniMax-M3","usage":null,
                 "choices":[{"index":0,"message":{"role":"assistant","tool_calls":[{"id":"c1","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#
        )));
    }

    /// 复现 MiniMax 返回 `finish_reason: ""`（空串）时，整块流不应解析失败，
    /// 空串应被宽容为 `None`。回归：修复前会抛 unknown variant 反序列化错误。
    #[test]
    fn stream_chunk_accepts_empty_finish_reason() {
        let json = r#"{
            "id":"06ec3869118c546b84014f6f29256030",
            "choices":[{"finish_reason":"","index":0,"delta":{"content":"tion: 比如","role":"assistant"}}],
            "created":1788675433,
            "model":"MiniMax-M3",
            "object":"chat.completion.chunk",
            "usage":null,
            "service_tier":"standard"
        }"#;
        let chunk: CompatChatStreamChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.choices[0].finish_reason, None);
    }

    /// 复现 MiniMax 流式 tool_calls 增量**完全省略** `type`/`id`（只带 `function.arguments`
    /// 与 `index`，即本次 `missing field type` 报错）时，整块也不应解析失败而被丢弃，
    /// arguments 得以保留——缺失字段经 `#[serde(default)]` 取 None 而非抛错。
    #[test]
    fn stream_chunk_accepts_tool_call_without_type_field() {
        let json = r#"{
            "id":"06eca834da48db93d47c1bad0324e5a2",
            "choices":[{"finish_reason":"tool_calls","index":0,"delta":{"role":"assistant","tool_calls":[
                {"function":{"arguments":"\"questions\": [{\"header\": \"plan\", \"question\": \"\u8bf7\u9009\u62e9\uff0c\uff1a\", \"options\": [{\"label\": \"A\", \"value\": \"A\"}]}]}"},"index":0}
            ]}}],
            "created":1788704074,
            "model":"MiniMax-M3",
            "object":"chat.completion.chunk",
            "usage":null,
            "service_tier":"standard"
        }"#;
        let chunk: CompatChatStreamChunk = serde_json::from_str(json)
            .unwrap_or_else(|e| panic!("省略 type/id 字段的 tool_calls 分片不应使整块解析失败: {}", e));
        let calls = chunk.choices[0]
            .delta
            .tool_calls
            .as_ref()
            .expect("应有 tool_calls");
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        // 缺失的 type/id 应宽容为 None（修复核心）
        assert!(call.r#type.is_none(), "缺失 type 应宽容为 None");
        assert!(call.id.is_none(), "缺失 id 应宽容为 None");
        // 含真实参数内容的 arguments 保留，供上层按 index 累积（增量片段，无需也不应单独完整解析）
        let func = call.function.as_ref().expect("应有 function");
        let raw_args = func.arguments.as_deref().unwrap_or("");
        assert!(raw_args.contains("questions"), "arguments 应保留真实参数片段: {}", raw_args);
    }

    /// 复现 MiniMax 流式 tool_calls 增量带 `"type":""`/`"id":""` 占位（含真实 arguments）
    /// 时，整块不应解析失败而被丢弃——空串 `type` 应被宽容为 `None`，arguments 得以保留。
    /// 回归：修复前 `FunctionType` 枚举解析 `""` 抛 unknown variant，导致含参数的分片被吞，
    /// 最终 `request_user_action` 的 questions 为空、前端无卡片可交互而永久卡住。
    #[test]
    fn stream_chunk_accepts_empty_tool_call_type_placeholder() {
        let json = r#"{
            "id":"06ec40829eaeb000968187b3d8e4efe8",
            "choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[
                {"id":"","type":"","function":{"name":"","arguments":"{\"message\":\"需求已明确\",\"questions\":[{\"header\":\"格式\",\"question\":\"请选择输出格式\",\"options\":[{\"label\":\"JSON\",\"value\":\"json\"}]}]}"},"index":0}
            ]}}],
            "created":1788677506,
            "model":"MiniMax-M3",
            "object":"chat.completion.chunk",
            "usage":null,
            "service_tier":"standard"
        }"#;
        let chunk: CompatChatStreamChunk = serde_json::from_str(json)
            .unwrap_or_else(|e| panic!("含空串 type 的 tool_calls 分片不应使整块解析失败: {}", e));
        let calls = chunk.choices[0]
            .delta
            .tool_calls
            .as_ref()
            .expect("应有 tool_calls");
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        // 空串 type 宽容为 None（修复核心）
        assert!(call.r#type.is_none(), "空串 type 应宽容为 None");
        // 含真实参数内容的 arguments 保留，供上层按 index 累积
        let func = call.function.as_ref().expect("应有 function");
        let args: serde_json::Value =
            serde_json::from_str(func.arguments.as_deref().unwrap_or("")).unwrap();
        assert!(args["questions"].is_array());
    }

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
    /// content 可能为 null。现在该字段会被反序列化并透传给 Message。
    #[test]
    fn deepseek_response_parses_reasoning_content() {
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
        // reasoning_content 被反序列化
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("先拆解问题，再给出结论……")
        );
        assert_eq!(resp.choices[0].finish_reason, Some(async_openai::types::chat::FinishReason::Stop));
    }

    /// DeepSeek 流式 delta 同样带 `reasoning_content`，现在会被反序列化。
    #[test]
    fn deepseek_stream_chunk_parses_reasoning_content() {
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
        assert_eq!(chunk.choices[0].delta.reasoning_content.as_deref(), Some("思考中……"));
        assert_eq!(chunk.choices[0].delta.role, Some(async_openai::types::chat::Role::Assistant));
    }

    /// <think> 标签拆分：去标签、单段、跨 chunk、无标签透传。
    #[test]
    fn split_think_content_handles_think_tags() {
        // 无标签：整段 content
        let (r, c) = split_think_content("纯文本", &mut false);
        assert_eq!(r, "");
        assert_eq!(c, "纯文本");

        // 单段内含完整 think 块 + 尾部 JSON
        let (r, c) = split_think_content("<think>先分析</think>{\"a\":1}", &mut false);
        assert_eq!(r, "先分析");
        assert_eq!(c, "{\"a\":1}");

        // 跨 chunk：<think> 未闭合 → 进入 think；下一 chunk 闭合
        let mut in_think = false;
        let (r1, c1) = split_think_content("<think>正在思考步骤一，", &mut in_think);
        assert!(in_think);
        assert_eq!(r1, "正在思考步骤一，");
        assert_eq!(c1, "");
        let (r2, c2) = split_think_content("继续想</think>  {\"b\":2}", &mut in_think);
        assert!(!in_think);
        assert_eq!(r2, "继续想");
        assert_eq!(c2, "  {\"b\":2}");
    }

    /// convert_response：content 含 <think> 标签时，clean 进 content、思考进 reasoning_content。
    #[test]
    fn convert_response_strips_think_tag() {
        let client = OpenAiClient::new(OpenAiClientConfig {
            api_key: "test-key".into(),
            model: "test-model".into(),
            base_url: None,
            default_temperature: None,
            default_max_tokens: None,
            organization: None,
            thinking_config: None,
        });
        let json = r#"{
            "id":"cmpl-t","object":"chat.completion","created":0,"model":"m",
            "choices":[{"index":0,"message":{"role":"assistant","content":"<think>分析中</think>  {\"keyword\":\"x\"}"},"finish_reason":"stop"}],
            "usage":null
        }"#;
        let resp: CompatChatResponse = serde_json::from_str(json).unwrap();
        let out = client.convert_response(resp).unwrap();
        let msg = &out.choices[0].message;
        match &msg.content {
            Some(MessageContent::Text { text }) => assert_eq!(text, "  {\"keyword\":\"x\"}"),
            other => panic!("unexpected content: {:?}", other),
        }
        assert_eq!(msg.reasoning_content.as_deref(), Some("分析中"));
    }

    // ─── 多模态：图片 → content array ──────────────────────────────

    use async_openai::types::chat::{
        ChatCompletionRequestMessage, ChatCompletionRequestUserMessageContent,
        ChatCompletionRequestUserMessageContentPart,
    };
    use planned_agent_core::ai::types::ImageDetail;

    fn test_client() -> OpenAiClient {
        OpenAiClient::new(OpenAiClientConfig {
            api_key: "test-key".into(),
            model: "test-model".into(),
            base_url: None,
            default_temperature: None,
            default_max_tokens: None,
            organization: None,
            thinking_config: None,
        })
    }

    /// 临时图片路径（进程 id + 时间戳，避免并发测试互踩）。
    fn temp_image_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "pa_ai_openai_{}_{}_{}.png",
            tag,
            std::process::id(),
            nanos
        ))
    }

    fn user_request(content: MessageContent) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "test-model".into(),
            messages: vec![Message {
                role: MessageRole::User,
                content: Some(content),
                ..Default::default()
            }],
            tools: None,
            temperature: None,
            max_tokens: None,
            stream: false,
            extra: HashMap::new(),
        }
    }

    /// 取转换后 User 消息的 `Array` 内容（并在形态不符时 panic）。
    fn expect_user_parts(
        converted: ChatCompletionRequestMessage,
    ) -> Vec<ChatCompletionRequestUserMessageContentPart> {
        match converted {
            ChatCompletionRequestMessage::User(u) => match u.content {
                ChatCompletionRequestUserMessageContent::Array(parts) => parts,
                other => panic!("expected Array content, got {other:?}"),
            },
            other => panic!("expected User message, got {other:?}"),
        }
    }

    /// 纯文本不回归：仍是 `content: "..."`，不变成 array。
    #[test]
    fn user_text_message_still_plain_string() {
        let client = test_client();
        let req = user_request(MessageContent::Text { text: "你好".into() });
        match client.convert_message(&req.messages[0]).unwrap() {
            ChatCompletionRequestMessage::User(u) => match u.content {
                ChatCompletionRequestUserMessageContent::Text(t) => assert_eq!(t, "你好"),
                other => panic!("expected plain text, got {other:?}"),
            },
            other => panic!("expected User message, got {other:?}"),
        }
    }

    /// 图文混排 + 本地文件：读盘 → base64 `data:` URL，与文本拼成 content array。
    #[tokio::test]
    async fn user_parts_text_plus_local_image_becomes_content_array() {
        let client = test_client();
        let path = temp_image_path("parts");
        let png_bytes: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];
        std::fs::write(&path, png_bytes).unwrap();

        let mut req = user_request(MessageContent::Parts {
            parts: vec![
                ContentPart::Text {
                    text: "这张图里有什么？".into(),
                },
                ContentPart::Image {
                    image: ImageSource::File {
                        path: path.clone(),
                        detail: Some(ImageDetail::Auto),
                    },
                },
            ],
        });

        resolve_local_images(&mut req).await.unwrap();
        let converted = client.convert_message(&req.messages[0]).unwrap();
        std::fs::remove_file(&path).ok();

        let parts = expect_user_parts(converted);
        assert_eq!(parts.len(), 2);
        match &parts[0] {
            ChatCompletionRequestUserMessageContentPart::Text(t) => {
                assert_eq!(t.text, "这张图里有什么？")
            }
            other => panic!("expected text part, got {other:?}"),
        }
        match &parts[1] {
            ChatCompletionRequestUserMessageContentPart::ImageUrl(img) => {
                assert!(
                    img.image_url.url.starts_with("data:image/png;base64,"),
                    "url={}",
                    img.image_url.url
                );
                use base64::Engine as _;
                let b64 = img
                    .image_url
                    .url
                    .trim_start_matches("data:image/png;base64,");
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .unwrap();
                assert_eq!(decoded, png_bytes);
                assert!(matches!(
                    img.image_url.detail,
                    Some(async_openai::types::chat::ImageDetail::Auto)
                ));
            }
            other => panic!("expected image part, got {other:?}"),
        }
    }

    /// `http(s)` URL 变成 image part，**不再**降级成 `"Image: {url}"` 文本。
    #[test]
    fn http_image_url_becomes_image_part_not_text() {
        let client = test_client();
        let req = user_request(MessageContent::Image {
            image: ImageSource::Url {
                url: "https://example.com/a.png".into(),
                detail: None,
            },
        });
        let parts = expect_user_parts(client.convert_message(&req.messages[0]).unwrap());
        assert_eq!(parts.len(), 1);
        match &parts[0] {
            ChatCompletionRequestUserMessageContentPart::ImageUrl(img) => {
                assert_eq!(img.image_url.url, "https://example.com/a.png");
                assert!(img.image_url.detail.is_none());
            }
            other => panic!("image 不得降级为文本，got {other:?}"),
        }
    }

    /// 已经是 `data:` 的 URL 原样透传（不读盘、不改写）。
    #[test]
    fn existing_data_url_passes_through() {
        let client = test_client();
        let url = "data:image/jpeg;base64,AAAA";
        let req = user_request(MessageContent::Image {
            image: ImageSource::Url {
                url: url.into(),
                detail: None,
            },
        });
        let parts = expect_user_parts(client.convert_message(&req.messages[0]).unwrap());
        match &parts[0] {
            ChatCompletionRequestUserMessageContentPart::ImageUrl(img) => {
                assert_eq!(img.image_url.url, url)
            }
            other => panic!("expected image part, got {other:?}"),
        }
    }

    /// detail 映射：core `Low/High/Auto` → 库同名变体。
    #[test]
    fn image_detail_is_mapped() {
        let client = test_client();
        let req = user_request(MessageContent::Image {
            image: ImageSource::Url {
                url: "https://example.com/a.png".into(),
                detail: Some(ImageDetail::High),
            },
        });
        let parts = expect_user_parts(client.convert_message(&req.messages[0]).unwrap());
        match &parts[0] {
            ChatCompletionRequestUserMessageContentPart::ImageUrl(img) => assert!(matches!(
                img.image_url.detail,
                Some(async_openai::types::chat::ImageDetail::High)
            )),
            other => panic!("expected image part, got {other:?}"),
        }
    }

    /// 未解析的 `File` 走到转换 → 内部错误（防预处理被绕过，防静默降级）。
    #[test]
    fn unresolved_file_source_is_internal_error() {
        let client = test_client();
        let req = user_request(MessageContent::Image {
            image: ImageSource::File {
                path: std::path::PathBuf::from(r"D:\nope\a.png"),
                detail: None,
            },
        });
        let err = client
            .convert_message(&req.messages[0])
            .unwrap_err()
            .to_string();
        assert!(err.contains("was not resolved"), "unexpected error: {err}");
    }

    /// 文件不存在 → Err，不静默丢图；错误链里带消息定位。
    #[tokio::test]
    async fn missing_local_image_errors() {
        let path = temp_image_path("missing");
        let mut req = user_request(MessageContent::Image {
            image: ImageSource::File {
                path: path.clone(),
                detail: None,
            },
        });
        let err = resolve_local_images(&mut req).await.unwrap_err();
        // `{:#}` 打印完整 cause 链（`to_string()` 只给最外层 context）
        let chain = format!("{err:#}");
        assert!(
            chain.contains("cannot read local image"),
            "unexpected error: {chain}"
        );
        assert!(chain.contains("message[0]"), "缺少消息定位: {chain}");
    }

    /// 扩展名不在白名单 → Err；白名单映射正确（含大小写）。
    #[tokio::test]
    async fn unsupported_image_extension_errors() {
        let path = temp_image_path("bad").with_extension("txt");
        std::fs::write(&path, b"not an image").unwrap();
        let err = local_image_to_data_url(&path).await.unwrap_err().to_string();
        std::fs::remove_file(&path).ok();
        assert!(
            err.contains("unsupported local image extension"),
            "unexpected error: {err}"
        );

        assert_eq!(image_mime_of(std::path::Path::new("a.PNG")), Some("image/png"));
        assert_eq!(
            image_mime_of(std::path::Path::new("a.jpeg")),
            Some("image/jpeg")
        );
        assert_eq!(
            image_mime_of(std::path::Path::new("a.webp")),
            Some("image/webp")
        );
        assert_eq!(image_mime_of(std::path::Path::new("a.gif")), Some("image/gif"));
        assert_eq!(image_mime_of(std::path::Path::new("a.bmp")), None);
    }

    /// 超过 20 MB 上限 → Err（纯函数，避免真造 20 MB 文件）。
    #[test]
    fn oversized_image_errors() {
        let path = std::path::Path::new("big.png");
        assert!(check_image_size(MAX_IMAGE_BYTES, path).is_ok());
        let err = check_image_size(MAX_IMAGE_BYTES + 1, path)
            .unwrap_err()
            .to_string();
        assert!(err.contains("too large"), "unexpected error: {err}");
    }

    /// 空 `Parts` 会被拒（`Array([])` 发出去只会吃 400）。
    #[test]
    fn empty_parts_is_rejected() {
        let client = test_client();
        let req = user_request(MessageContent::Parts { parts: vec![] });
        let err = client
            .convert_message(&req.messages[0])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("empty content parts"),
            "unexpected error: {err}"
        );
    }

    /// 图片只支持 user 角色：System 携带图片 → 明确报错（而不是含混的 "must have text"）。
    #[test]
    fn image_on_system_role_is_rejected() {
        let client = test_client();
        let msg = Message {
            role: MessageRole::System,
            content: Some(MessageContent::Image {
                image: ImageSource::Url {
                    url: "https://example.com/a.png".into(),
                    detail: None,
                },
            }),
            ..Default::default()
        };
        let err = client.convert_message(&msg).unwrap_err().to_string();
        assert!(
            err.contains("only supported on user messages"),
            "unexpected error: {err}"
        );
    }
}

