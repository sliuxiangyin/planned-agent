# 核心抽象层 (`crates/core`)

定义与具体实现无关的 trait、数据类型和错误边界，为 AI、MCP、提示管理、工具注册和计划执行组件提供稳定的跨 crate 契约。核心层不负责调用外部服务，也不包含具体的模型或工具实现。

## 目录结构

```text
crates/core/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── types.rs             # AI、消息、工具和计划上下文等通用类型
    ├── ai/
    │   ├── mod.rs
    │   └── traits.rs        # AiClient 与流式响应抽象
    ├── errors/
    │   ├── mod.rs
    │   └── error_types.rs   # 核心错误类型
    ├── events/
    │   ├── mod.rs
    │   └── event_types.rs   # 事件类型
    ├── mcp/
    │   ├── mod.rs
    │   └── traits.rs        # MCP 管理器抽象
    ├── planner/
    │   ├── coarse/         # 粗粒度计划接口和类型
    │   ├── react/           # ReAct 接口和类型
    │   ├── replanner/      # 重规划类型
    │   └── validation/     # 计划验证类型
    ├── prompt/
    │   ├── mod.rs
    │   └── traits.rs        # PromptManager 抽象
    └── tool_registry/
        ├── mod.rs
        ├── traits.rs        # 工具执行和提供者抽象
        └── types.rs         # 工具分类及相关类型
```

`factory/` 目录仍作为扩展预留，但当前主流程直接通过 `AiClient` 和 `AiManager` 注入客户端。

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

所有消息都使用统一的 `Message` 结构，并兼容普通文本、图片、工具调用和模型思考字段：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: Option<MessageContent>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
    pub reasoning_content: Option<String>,
}
```

`reasoning_content` 用于兼容支持思考模式的模型；不同适配器可以根据服务商能力选择填充或忽略该字段。

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

## 计划引擎抽象 (`planner/`)

计划引擎采用分层接口：粗粒度计划器负责定义步骤目标，ReAct Agent 负责执行单个步骤。重规划和验证目前主要提供数据类型，具体协调器尚未在 `planned-agent` crate 中实现。

### 粗粒度计划器接口

实现：[CoarsePlanner](../crates/core/src/planner/coarse/coarse_planner.rs)

```rust
#[async_trait]
pub trait CoarsePlanner: Send + Sync {
    async fn generate_coarse_plan(
        &self,
        input: &str,
        context: &PlanContext,
    ) -> Result<CoarseGrainedPlan>;

    async fn validate_coarse_plan(
        &self,
        plan: &CoarseGrainedPlan,
    ) -> Result<CoarsePlanValidationResult>;

    fn name(&self) -> &str;
}
```

相关类型位于 `planner/coarse/coarse_types.rs`：

- `CoarseGrainedPlan`：计划整体信息、步骤列表、复杂度和风险等级。
- `CoarseGrainedStep`：步骤意图、预期输出、结果引用、依赖和数据需求。
- `DataRequirement`：步骤所需输入数据。
- `CoarsePlanValidationResult`：计划验证的错误和警告。

### ReAct Agent 接口

实现：[ReActAgent](../crates/core/src/planner/react/react_trait.rs)

```rust
#[async_trait]
pub trait ReActAgent: Send + Sync {
    async fn execute_coarse_step(
        &self,
        coarse_step: &CoarseGrainedStep,
        context: &PlanContext,
    ) -> Result<ReActExecutionResult>;

    async fn think(
        &self,
        coarse_step: &CoarseGrainedStep,
        history: &[ReActStep],
        context: &PlanContext,
        remaining_steps: Option<&[CoarseGrainedStep]>,
    ) -> Result<Thought>;

    async fn act(&self, thought: &Thought, context: &PlanContext) -> Result<Action>;
    async fn execute_tool(&self, action: &Action) -> Result<Observation>;
    async fn observe(&self, coarse_step: &CoarseGrainedStep, observation: &Observation) -> Result<ObserveResult>;
    fn is_complete(&self, observation: &Observation) -> bool;
    fn name(&self) -> &str;
}
```

ReAct 类型位于 `planner/react/react_types.rs`：

- `Thought`：思考结果和下一步计划。
- `Action`：工具名称、参数和选择理由。
- `Observation`：工具输出、错误和耗时。
- `ReActStep`：一轮思考、行动、观察历史。
- `ReActExecutionResult`：单个粗粒度步骤的最终执行结果。
- `ReActAgentConfig`：迭代、超时和重试相关配置。

### 重规划与验证类型

- `planner/replanner/replanner_types.rs` 定义重规划请求、响应和重规划动作。
- `planner/validation/validation_types.rs` 定义计划验证错误、警告和验证结果。
- 当前没有对应的完整实现组件，调用方不能将这些类型误认为已经存在的协调器。

## MCP 客户端 trait (`mcp/traits.rs`)

MCP 抽象负责连接管理、工具发现、工具调用、健康检查和重连。具体的 `McpManager` 实现在 `mcp-rmcp` crate 中。

## Prompt 管理 trait (`prompt/traits.rs`)

Prompt 抽象负责模板加载、渲染、响应解析和响应验证。具体实现位于 `prompt-manager` crate。

## 工具注册抽象 (`tool_registry/`)

工具注册抽象定义工具执行器、内置工具提供者、工具分类和工具调用相关类型。具体注册、路由和参数验证逻辑位于 `tool-manager` crate。

## 设计约束

1. 核心接口保持与具体 AI SDK、MCP SDK 和工具实现无关。
2. 异步 trait 对象需要满足 `Send + Sync`，便于在 Tokio 任务间共享。
3. 跨层传递的数据使用 `serde` 和 `serde_json`，方便 Prompt、工具和 API 适配。
4. 组件实现应依赖核心 trait，而不是依赖另一个具体实现 crate。