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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// 统一消息结构（符合 OpenAI 标准）
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// MCP 工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// MCP 工具调用结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: Value,
    pub is_error: bool,
}

/// MCP 服务器配置（支持多个）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub server_command: String,
    pub server_args: Vec<String>,
    pub transport: String,
    pub timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
    pub is_default: bool,
    pub tools_filter: Option<Vec<String>>,
    /// 工具分类（可选，用于分类过滤）
    #[serde(default)]
    pub categories: Option<Vec<String>>,
}

/// 思考模式配置（适用于支持思考模式的AI模型）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// 思考模式开关：enabled/disabled
    pub enabled: bool,
    /// 思考强度：high/max（默认high，复杂Agent类请求自动设置为max）
    pub effort: Option<String>,
}

/// AI 提供商配置（支持多个）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiProviderConfig {
    pub name: String,
    pub provider: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub base_url: Option<String>,
    pub is_default: bool,
    /// 思考模式配置（适用于支持思考模式的AI模型）
    pub thinking_config: Option<ThinkingConfig>,
}

/// 连接状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub last_ping: Option<chrono::DateTime<chrono::Utc>>,
    pub error_count: u32,
    pub uptime_secs: u64,
}

/// 计划上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanContext {
    /// 用户ID，用于多用户场景下的个性化计划生成和权限控制
    pub user_id: Option<String>,
    /// 会话ID，用于跟踪一次完整的对话/任务流程，支持会话级别的上下文保持
    pub session_id: Option<String>,
    /// 历史记录，存储之前的对话或操作历史，用于上下文理解
    pub history: Vec<String>,
    /// 扩展元数据，存储无法预定义的动态数据，支持灵活的业务扩展
    /// 常见字段：language, priority, timeout_ms, max_steps, user_role 等
    pub metadata: HashMap<String, Value>,
}

/// 计划
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub title: String,
    pub description: String,
    pub steps: Vec<PlanStep>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 计划步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub order: u32,
    pub action: String,
    pub parameters: Value,
    pub dependencies: Vec<String>,
    pub status: PlanStepStatus,
}

/// 计划步骤状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Skipped,
}

/// 计划执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecution {
    pub plan_id: String,
    pub status: PlanExecutionStatus,
    pub results: Vec<PlanStepResult>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 计划执行状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlanExecutionStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// 计划步骤结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStepResult {
    pub step_id: String,
    pub success: bool,
    pub output: Value,
    pub error: Option<String>,
    pub duration_ms: u64,
}
