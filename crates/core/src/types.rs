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

/// UI 交互动作 —— Agent 通过 tool call 请求前端渲染交互组件。
///
/// 当 `ChatService` 检测到 `request_user_action` tool call 时，
/// 会将其参数解析为此结构，并通过 `ChatEvent::UIActionRequest` 下发到前端。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UIAction {
    /// 动作唯一标识（如 "generate_plan", "add_more_detail"）
    pub id: String,
    /// 动作类型
    #[serde(rename = "type")]
    pub action_type: UIActionType,
    /// 展示文本（按钮文字、选项标签）
    pub label: String,
    /// 补充说明（可选 tooltip / 副文本）
    #[serde(default)]
    pub description: Option<String>,
}

/// UI 交互动作类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UIActionType {
    /// 确认按钮——用于"是/否"或多选一场景（如"生成计划"/"我再想想"）
    Confirm,
    /// 单选列表——从多个选项中选一个（如选择分析类型）
    Select,
    /// 文本输入提示——引导用户输入具体信息（如路径、关键词）
    Input,
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
    /// 最近一次连接失败的结构化错误（成功连接后会被清空）。
    /// UI 层据此展示失败原因；当前未消费，保留作为数据出口。
    #[serde(default)]
    pub last_error: Option<ConnectionError>,
}

/// 连接失败原因分类
///
/// 用途：MCP 客户端 `connect()` 失败时按失败阶段分类记录，
/// 供上层（UI、监控、Agent 自愈）按类别决定处理策略。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionError {
    /// 启动/握手总耗时超过 `timeout_secs`。
    /// 覆盖完整冷启动链：spawn 子进程 → npx 拉包 → 真实 MCP server 启动 → initialize 握手。
    /// 首次 `npx -y <pkg>` 下载可能耗时数十秒，需要给足余量。
    Timeout {
        /// 实际等待的秒数
        elapsed_secs: u64,
        /// 配置的超时上限（秒）
        timeout_secs: u64,
        /// 子进程 stderr 末尾输出（如果可读且非空）。
        /// 当子进程启动后被超时打断时，这里通常包含真实的报错原因（如 `MODULE_NOT_FOUND`）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_tail: Option<String>,
    },
    /// 无法启动子进程：命令不存在、权限不足等。
    /// 此阶段子进程未运行，无 stderr 可捕获。
    Spawn {
        reason: String,
    },
    /// 进程已启动但 MCP initialize 握手失败（如子进程崩溃、协议错误）。
    /// `stderr_tail` 携带子进程 stderr 末尾输出，便于 UI 展示真实失败原因。
    Handshake {
        reason: String,
        /// 子进程 stderr 末尾输出（如果可读且非空）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_tail: Option<String>,
    },
}

impl ConnectionError {
    /// 机器可读的失败分类（用于持久化 / IPC / UI 标签）
    ///
    /// 返回字符串与 serde `tag` 字段保持一致（`"timeout"` / `"spawn"` / `"handshake"`），
    /// 便于后续 JSON 字段直读。
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Timeout { .. } => "timeout",
            Self::Spawn { .. } => "spawn",
            Self::Handshake { .. } => "handshake",
        }
    }

    /// 供 UI / 日志展示的人类可读消息
    pub fn message(&self) -> String {
        let base = match self {
            Self::Timeout { elapsed_secs, timeout_secs, .. } => format!(
                "MCP server startup timed out after {}s (limit {}s). \
                 npx package download or handshake may be slow; \
                 consider raising `timeout_secs` in the server config.",
                elapsed_secs, timeout_secs
            ),
            Self::Spawn { reason } => {
                format!("Failed to spawn MCP server process: {}", reason)
            }
            Self::Handshake { reason, .. } => {
                format!("MCP handshake failed: {}", reason)
            }
        };

        // 追加子进程 stderr 末尾（让用户看到真实失败原因）
        let stderr = match self {
            Self::Timeout { stderr_tail, .. } => stderr_tail.as_ref(),
            Self::Handshake { stderr_tail, .. } => stderr_tail.as_ref(),
            Self::Spawn { .. } => None,
        };
        match stderr {
            Some(s) if !s.trim().is_empty() => {
                format!("{}\n\n--- subprocess stderr ---\n{}", base, s.trim_end())
            }
            _ => base,
        }
    }
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
