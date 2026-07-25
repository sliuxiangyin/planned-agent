# AI SDK 适配器 (`crates/ai-openai`)

实现 `AiClient` trait，封装 async-openai 库。

## 目录结构

```
crates/ai-openai/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── client.rs      # OpenAI 客户端实现
    └── streaming.rs   # 流式响应处理
```

## 关键功能

- 支持 OpenAI 和兼容 API（通过配置 base_url）
- 实现流式响应：使用 `client.chat().create_stream()` 方法
- 支持消息转换（系统消息、用户消息、助手消息、工具消息）
- 支持工具调用链
- 错误处理：统一错误类型，包含重试逻辑
- 动态调度：利用 async-openai 的 `Box<dyn Config>` 特性

## 客户端配置

```rust
pub struct OpenAiClientConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: Option<String>,
    pub default_temperature: Option<f32>,
    pub default_max_tokens: Option<u32>,
    pub organization: Option<String>,
}
```

## 消息转换

### 系统消息转换

```rust
// 系统消息转换为 OpenAI 格式
async_openai::types::ChatCompletionRequestMessage::System(
    async_openai::types::ChatCompletionRequestSystemMessage {
        content: async_openai::types::ChatCompletionRequestSystemMessageContent::Text(content),
        name: message.name.clone(),
    }
)
```

### 用户消息转换

```rust
// 用户消息转换为 OpenAI 格式（支持文本和图片）
async_openai::types::ChatCompletionRequestMessage::User(
    async_openai::types::ChatCompletionRequestUserMessage {
        content: async_openai::types::ChatCompletionRequestUserMessageContent::Text(text),
        name: message.name.clone(),
    }
)
```

### 助手消息转换

```rust
// 助手消息转换为 OpenAI 格式（支持工具调用）
async_openai::types::ChatCompletionRequestMessage::Assistant(
    async_openai::types::ChatCompletionRequestAssistantMessage {
        content: Some(async_openai::types::ChatCompletionRequestAssistantMessageContent::Text(text)),
        tool_calls: Some(openai_tool_calls),
        name: message.name.clone(),
        ..Default::default()
    }
)
```

### 工具消息转换

```rust
// 工具消息转换为 OpenAI 格式
async_openai::types::ChatCompletionRequestMessage::Tool(
    async_openai::types::ChatCompletionRequestToolMessage {
        tool_call_id,
        content: async_openai::types::ChatCompletionRequestToolMessageContent::Text(content),
    }
)
```

## 流式输出实现要点

```rust
use async_openai::{Client, config::OpenAIConfig};
use futures::StreamExt;

pub struct OpenAiClient {
    client: Client<OpenAIConfig>,
    config: OpenAiClientConfig,
}

impl OpenAiClient {
    pub fn new(config: OpenAiClientConfig) -> Self {
        let mut openai_config = OpenAIConfig::new()
            .with_api_key(&config.api_key);
        
        if let Some(base_url) = &config.base_url {
            openai_config = openai_config.with_api_base(base_url);
        }
        
        let client = Client::with_config(openai_config);
        Self { client, config }
    }
}

#[async_trait]
impl AiClient for OpenAiClient {
    async fn chat_completion(&self, request: ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        let chat_request = self.convert_request(&request)?;
        
        // 添加重试逻辑
        let mut retries = 0;
        let max_retries = 3;
        
        loop {
            match self.client.chat().create(chat_request.clone()).await {
                Ok(response) => {
                    return self.convert_response(response);
                }
                Err(e) => {
                    retries += 1;
                    if retries >= max_retries {
                        return Err(e.into());
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
    }
    
    async fn chat_completion_stream(&self, request: ChatCompletionRequest) -> Result<ChatCompletionStream> {
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
```

## 工具调用处理

### 工具定义转换

```rust
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
```

### 工具调用响应转换

```rust
// 将 OpenAI 工具调用转换为内部格式
let tool_calls = choice.message.tool_calls.as_ref().map(|calls| {
    calls.iter().map(|call| ToolCall {
        id: call.id.clone(),
        r#type: ToolType::Function,
        function: FunctionCall {
            name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
        },
    }).collect()
});
```

## 错误处理与重试

适配器应实现统一的错误类型，并为网络请求、流式处理等操作提供重试机制。建议使用指数退避策略，避免频繁请求导致服务端限流。

当前实现使用固定延迟重试（1秒），最大重试次数为3次。

## 配置

通过 `OpenAiClientConfig` 配置 API 密钥、模型名称、基础 URL 等参数。支持多配置切换，便于使用不同的 AI 提供商。

### 从会话创建客户端

```rust
impl OpenAiClient {
    pub fn from_conversation(conversation: &Conversation, api_key: String) -> Self {
        let config = OpenAiClientConfig {
            api_key,
            model: conversation.model.clone(),
            base_url: None,
            default_temperature: conversation.temperature,
            default_max_tokens: conversation.max_tokens,
            organization: None,
        };
        Self::new(config)
    }
}
```
# AI SDK 适配器 (`crates/ai-openai`)

实现 `AiClient` trait，封装 async-openai 库。

## 目录结构

```
crates/ai-openai/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── client.rs      # OpenAI 客户端实现
    └── streaming.rs   # 流式响应处理
```

## 关键功能

- 支持 OpenAI 和兼容 API（通过配置 base_url）
- 实现流式响应：使用 `client.chat().create_stream()` 方法
- 动态调度：利用 async-openai 的 `Box<dyn Config>` 特性
- 错误处理：统一错误类型，包含重试逻辑

## 流式输出实现要点

```rust
use async_openai::{Client, config::OpenAIConfig};
use futures::StreamExt;

pub struct OpenAiClient {
    client: Client<OpenAIConfig>,
}

impl OpenAiClient {
    pub fn new(api_key: &str) -> Self {
        let config = OpenAIConfig::new().with_api_key(api_key);
        let client = Client::with_config(config);
        Self { client }
    }
}

#[async_trait]
impl AiClient for OpenAiClient {
    async fn stream(&self, request: AiRequest) -> Result<AiStreamResponse> {
        let openai_request = self.convert_request(request)?;
        let mut stream = self.client.chat().create_stream(openai_request).await?;
        
        // 将 OpenAI 流转换为通用流
        let mapped_stream = stream.map(|chunk| {
            chunk.map(|c| self.convert_chunk(c))
                 .map_err(|e| AiError::StreamError(e.to_string()))
        });
        
        Ok(AiStreamResponse::new(mapped_stream))
    }
}
```

## 错误处理与重试

适配器应实现统一的错误类型，并为网络请求、流式处理等操作提供重试机制。建议使用指数退避策略，避免频繁请求导致服务端限流。

## 配置

通过 `AiProviderConfig` 配置 API 密钥、模型名称、基础 URL 等参数。支持多配置切换，便于使用不同的 AI 提供商。