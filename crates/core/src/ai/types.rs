use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 消息内容类型（符合 OpenAI 标准）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
    /// 文本内容
    Text {
        text: String,
    },
    /// 图片内容
    Image {
        image_url: ImageUrl,
    },
    /// 工具调用结果
    ToolResult {
        tool_call_id: String,
        content: String,
    },
}

impl Default for MessageContent {
    /// 默认空文本（GUI 构造占位消息时常用）
    fn default() -> Self {
        MessageContent::Text {
            text: String::new(),
        }
    }
}

/// 图片 URL
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    pub detail: Option<ImageDetail>,
}

/// 图片细节级别
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    Low,
    High,
    Auto,
}

/// 工具类型
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolType {
    Function,
}

/// 函数调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// 消息角色（符合 OpenAI 标准）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    #[default]
    User,
    System,
    Assistant,
    Tool,
}

/// 统一消息结构（符合 OpenAI 标准）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Message {
    /// 消息角色
    pub role: MessageRole,
    /// 消息内容（支持多种类型）
    pub content: Option<MessageContent>,
    /// 工具调用列表（仅 assistant 角色）
    pub tool_calls: Option<Vec<ToolCall>>,
    /// 工具调用 ID（仅 tool 角色）
    pub tool_call_id: Option<String>,
    /// 名称（可选）
    pub name: Option<String>,
    /// 思考内容（思考模式下的思维链）
    pub reasoning_content: Option<String>,
}

/// 工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: ToolType,
    pub function: FunctionCall,
}

/// 对话上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    /// 消息列表
    pub messages: Vec<Message>,
    /// 模型名称
    pub model: String,
    /// 系统提示
    pub system_prompt: Option<String>,
    /// 工具定义
    pub tools: Option<Vec<ToolDefinition>>,
    /// 温度参数
    pub temperature: Option<f32>,
    /// 最大 token 数
    pub max_tokens: Option<u32>,
    /// 其他参数
    pub extra: HashMap<String, Value>,
}

/// 工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub r#type: ToolType,
    pub function: FunctionDefinition,
}

/// 函数定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: Option<String>,
    pub parameters: Option<Value>,
    pub strict: Option<bool>,
}

/// AI 请求（符合 OpenAI 标准）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    /// 模型名称
    pub model: String,
    /// 消息列表
    pub messages: Vec<Message>,
    /// 工具定义
    pub tools: Option<Vec<ToolDefinition>>,
    /// 温度参数
    pub temperature: Option<f32>,
    /// 最大 token 数
    pub max_tokens: Option<u32>,
    /// 是否流式输出
    pub stream: bool,
    /// 其他参数
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// AI 响应（符合 OpenAI 标准）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    /// 响应 ID
    pub id: String,
    /// 对象类型
    pub object: String,
    /// 创建时间
    pub created: u64,
    /// 模型名称
    pub model: String,
    /// 选择列表
    pub choices: Vec<Choice>,
    /// 使用情况
    pub usage: Option<Usage>,
    /// 系统指纹
    pub system_fingerprint: Option<String>,
}

/// 选择
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    /// 索引
    pub index: u32,
    /// 消息
    pub message: Message,
    /// 完成原因
    pub finish_reason: Option<FinishReason>,
    /// 对数概率
    pub logprobs: Option<Value>,
}

/// 完成原因
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    FunctionCall,
}

/// 使用情况
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// 流式响应块（符合 OpenAI 标准）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    /// 响应 ID
    pub id: String,
    /// 对象类型
    pub object: String,
    /// 创建时间
    pub created: u64,
    /// 模型名称
    pub model: String,
    /// 选择列表
    pub choices: Vec<ChunkChoice>,
    /// 系统指纹
    pub system_fingerprint: Option<String>,
    /// 使用情况（可选）
    pub usage: Option<Usage>,
}

/// 流式选择
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkChoice {
    /// 索引
    pub index: u32,
    /// 增量消息
    pub delta: DeltaMessage,
    /// 完成原因
    pub finish_reason: Option<FinishReason>,
    /// 对数概率
    pub logprobs: Option<Value>,
}

/// 增量消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaMessage {
    /// 角色
    pub role: Option<MessageRole>,
    /// 内容
    pub content: Option<String>,
    /// 工具调用
    pub tool_calls: Option<Vec<DeltaToolCall>>,
    /// 思考内容（思考模式下的思维链）
    pub reasoning_content: Option<String>,
}

/// 增量工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaToolCall {
    /// 索引
    pub index: u32,
    /// ID
    pub id: Option<String>,
    /// 类型
    pub r#type: Option<ToolType>,
    /// 函数调用
    pub function: Option<DeltaFunctionCall>,
}

/// 增量函数调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaFunctionCall {
    /// 名称
    pub name: Option<String>,
    /// 参数
    pub arguments: Option<String>,
}
