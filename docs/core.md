# 核心抽象层 (`crates/core`)

定义与具体实现无关的 trait 和类型，确保可扩展性。

## 目录结构

```
crates/core/
├── Cargo.toml
└── src/
    ├── lib.rs         # 核心 trait 定义
    ├── ai/            # AI SDK 抽象
    │   ├── mod.rs
    │   ├── traits.rs  # AI 客户端 trait (支持流式/直接输出)
    │   └── types.rs   # 通用类型定义
    ├── factory/       # 客户端工厂
    │   ├── mod.rs
    │   └── ai_factory.rs  # AI 客户端工厂
    ├── mcp/           # MCP 集成抽象
    │   ├── mod.rs
    │   └── traits.rs  # MCP 客户端 trait
    └── planner/       # 计划引擎
        ├── mod.rs
        └── traits.rs  # 计划器 trait (占位)
```

## AI 客户端 trait (`ai/traits.rs`)

符合 OpenAI API 行业标准的 AI 客户端 trait：

```rust
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
```

### 流式响应包装器

```rust
pub struct ChatCompletionStream {
    pub stream: Box<dyn futures::Stream<Item = Result<ChatCompletionChunk>> + Send + Unpin>,
}
```

## 消息角色系统

系统支持四种标准消息角色（符合 OpenAI 标准）：

1. **System** - 系统提示，定义 AI 行为
2. **User** - 用户输入
3. **Assistant** - AI 回复
4. **Tool** - 工具调用结果

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}
```

## 消息内容类型

消息内容支持多种类型（符合 OpenAI 标准）：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
    /// 文本内容
    Text { text: String },
    /// 图片内容
    Image { image_url: ImageUrl },
    /// 工具调用结果
    ToolResult { tool_call_id: String, content: String },
}
```

## 统一消息结构

所有消息都使用统一的 `Message` 结构（符合 OpenAI 标准）：

```rust
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
}
```

## 请求和响应格式

### 请求格式

```rust
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
```

### 响应格式

```rust
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
```

### 流式响应块

```rust
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
```

## MCP 客户端 trait (`mcp/traits.rs`)

```rust
#[async_trait]
pub trait McpClient: Send + Sync {
    /// 连接到 MCP 服务器
    async fn connect(&mut self, config: McpConfig) -> Result<()>;
    
    /// 列出可用工具
    async fn list_tools(&self) -> Result<Vec<Tool>>;
    
    /// 调用工具
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult>;
    
    /// 断开连接
    async fn disconnect(&mut self) -> Result<()>;
    
    /// 检查连接是否健康
    async fn is_connected(&self) -> bool;
    
    /// 获取连接状态信息
    async fn connection_status(&self) -> ConnectionStatus;
    
    /// 心跳检测（如果服务器支持）
    async fn ping(&self) -> Result<()>;
    
    /// 重新连接
    async fn reconnect(&mut self) -> Result<()>;
}
```

## 计划器 trait (`planner/traits.rs`)

```rust
#[async_trait]
pub trait Planner: Send + Sync {
    /// 分析用户输入，生成计划步骤
    async fn create_plan(&self, input: &str, context: &PlanContext) -> Result<Plan>;
    
    /// 执行计划步骤
    async fn execute_plan(&self, plan: &Plan) -> Result<PlanExecution>;
}
```

## 详细计划类型 (`planner/detailed/detailed_types.rs`)

### 工具探索上下文

```rust
/// 依赖步骤信息
#[derive(Debug, Clone)]
pub struct DependencyStepInfo {
    /// 步骤ID
    pub step_id: String,
    /// 步骤意图
    pub intent: String,
    /// 使用的工具名称（如果有）
    pub tool_name: Option<String>,
}

/// 工具探索上下文
#[derive(Debug, Clone)]
pub struct ToolExplorationContext {
    /// 当前步骤的依赖步骤信息（上一步的意图和使用的工具）
    pub dependency_steps: Vec<DependencyStepInfo>,
    /// 整体计划描述
    pub plan_description: String,
}
```

**用途**：在工具探索阶段传递上下文信息，帮助LLM理解步骤间关系，避免误解步骤意图。

## 通用类型定义 (`ai/types.rs`)

该模块包含 AI 请求和响应等通用类型定义，用于在 trait 和实现之间传递数据。具体类型定义请参考实际代码实现。

## 工厂模式 (`factory/`)

工厂模块提供 AI 客户端的创建逻辑，支持根据配置动态实例化不同的 AI 客户端实现。
# 核心抽象层 (`crates/core`)

定义与具体实现无关的 trait 和类型，确保可扩展性。

## 目录结构

```
crates/core/
├── Cargo.toml
└── src/
    ├── lib.rs         # 核心 trait 定义
    ├── ai/            # AI SDK 抽象
    │   ├── mod.rs
    │   ├── traits.rs  # AI 客户端 trait (支持流式/直接输出)
    │   └── types.rs   # 通用类型定义
    ├── factory/       # 客户端工厂
    │   ├── mod.rs
    │   └── ai_factory.rs  # AI 客户端工厂
    ├── mcp/           # MCP 集成抽象
    │   ├── mod.rs
    │   └── traits.rs  # MCP 客户端 trait
    └── planner/       # 计划引擎
        ├── mod.rs
        └── traits.rs  # 计划器 trait (占位)
```

## AI 客户端 trait (`ai/traits.rs`)

```rust
#[async_trait]
pub trait AiClient: Send + Sync {
    /// 直接输出模式：发送请求，返回完整响应
    async fn complete(&self, request: AiRequest) -> Result<AiResponse>;
    
    /// 流式输出模式：发送请求，返回流式响应
    async fn complete_stream(&self, request: AiRequest) -> Result<AiStreamResponse>;
    
    /// 获取提供商名称
    fn provider_name(&self) -> &str;
    
    /// 获取模型名称
    fn model_name(&self) -> &str;
}
```

## MCP 客户端 trait (`mcp/traits.rs`)

```rust
#[async_trait]
pub trait McpClient: Send + Sync {
    /// 连接到 MCP 服务器
    async fn connect(&mut self, config: McpConfig) -> Result<()>;
    
    /// 列出可用工具
    async fn list_tools(&self) -> Result<Vec<Tool>>;
    
    /// 调用工具
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult>;
    
    /// 断开连接
    async fn disconnect(&mut self) -> Result<()>;
    
    /// 检查连接是否健康
    async fn is_connected(&self) -> bool;
    
    /// 获取连接状态信息
    async fn connection_status(&self) -> ConnectionStatus;
    
    /// 心跳检测（如果服务器支持）
    async fn ping(&self) -> Result<()>;
    
    /// 重新连接
    async fn reconnect(&mut self) -> Result<()>;
}
```

## 计划器 trait (`planner/traits.rs`)

```rust
#[async_trait]
pub trait Planner: Send + Sync {
    /// 分析用户输入，生成计划步骤
    async fn create_plan(&self, input: &str, context: &PlanContext) -> Result<Plan>;
    
    /// 执行计划步骤
    async fn execute_plan(&self, plan: &Plan) -> Result<PlanExecution>;
}
```

## 通用类型定义 (`ai/types.rs`)

该模块包含 AI 请求和响应等通用类型定义，用于在 trait 和实现之间传递数据。具体类型定义请参考实际代码实现。

## 工厂模式 (`factory/`)

工厂模块提供 AI 客户端的创建逻辑，支持根据配置动态实例化不同的 AI 客户端实现。