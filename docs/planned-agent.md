# 主程序 (`crates/planned-agent`)

整合所有模块，提供用户交互界面。

## 目录结构

```
crates/planned-agent/
├── Cargo.toml
└── src/
    ├── main.rs        # 入口点
    ├── config.rs      # 配置管理（支持多配置）
    ├── agent.rs       # 代理核心逻辑
    └── cli.rs         # 命令行接口
```

## 核心流程

1. 加载配置（AI 密钥、MCP 服务器地址等）
2. 创建 AI 客户端（通过工厂模式）
3. 连接 MCP 服务器并获取工具列表
4. 启动交互式命令行或 API 服务器
5. 处理用户输入：
   - 将输入发送给 AI，附加工具定义
   - 处理 AI 响应（可能包含工具调用）
   - 如果 AI 请求工具调用，则调用 MCP 工具
   - 将工具结果反馈给 AI
   - 返回最终响应

## 工厂模式实现

AI 客户端工厂负责根据配置创建对应的 AI 客户端实例。

**核心功能**：
- 根据提供商配置创建 AI 客户端
- 支持多提供商配置（OpenAI、Claude等）
- 提供默认客户端选择机制

> 详细实现见 [工厂模式设计文档](./planned-agent/factory-pattern.md)

## MCP 管理器

MCP 管理器负责管理多个 MCP 服务器连接和工具调用。

**核心功能**：
- 连接到单个/多个 MCP 服务器
- 工具调用（指定服务器或自动路由）
- 获取所有可用工具列表

> 详细实现见 [MCP管理器设计文档](./planned-agent/mcp-manager.md)

## 代理核心逻辑

### 处理用户输入

代理核心逻辑负责处理用户输入，包括：
1. 获取 AI 客户端
2. 获取所有工具定义
3. 转换工具定义格式
4. 发送给 AI（带工具定义）
5. 处理工具调用
6. 返回最终响应

### 处理工具调用

当 AI 响应包含工具调用时：
1. 自动路由到正确的 MCP 服务器
2. 执行工具调用
3. 将工具结果反馈给 AI
4. 获取最终响应

> 详细实现见 [代理核心逻辑设计文档](./planned-agent/agent-core.md)

## 计划引擎设计

基于LangChain的Plan-and-Execute模式，结合ReWOO、LLMCompiler等业界最佳实践，设计并实现完整的计划生成与执行系统。该系统将集成现有的MCP客户端、提示管理器和AI客户端，提供智能的任务分解、执行和监控能力。

### 核心设计理念

#### 1. 架构选择
**混合架构：宏观Plan-and-Execute + 微观ReWOO**

- **宏观层**：使用Plan-and-Execute模式处理复杂、探索性任务
  - 支持动态重规划
  - 适合未知环境和探索性任务
  - 容错能力强

- **微观层**：使用ReWOO模式处理确定性、可并行任务
  - 减少LLM调用次数
  - 支持并行执行
  - 成本效益高

#### 2. 核心原则
- **渐进式信息获取**：前面步骤的发现指导后面步骤的优化
- **动态重规划**：根据执行结果调整后续计划
- **工具探索**：不预先指定工具，让LLM在运行时探索
- **风险控制**：智能确认机制，防止高风险操作

#### 3. 分层实现原则
**实现顺序：Core层 → Planned-agent层 → 应用层**

- **Core层（接口定义）**：只定义trait和类型，不包含实现逻辑
  - 定义所有接口（trait）
  - 定义所有类型（struct、enum）
  - 定义错误类型
  - 定义事件类型

- **Planned-agent层（独立实现）**：每个模块独立实现，可单独测试
  - 粗粒度计划器（CoarsePlanner）
  - 详细步骤分析器（DetailedPlanner）
  - 执行器（Executor）
  - 重规划器（Replanner）
  - 记忆管理器（Memory）
  - 计划验证器（PlanValidator）

- **应用层（协调组织）**：最后实现，组织各模块协同工作
  - 监督器（Supervisor）
  - 协调器（PlanOrchestrator）

**优势**：
- **低耦合**：模块间通过接口通信，修改一个不影响其他
- **可测试**：每个模块可独立测试，快速发现问题
- **渐进式**：可以逐步实现，逐步集成，逐步优化

#### 4. 复用已实现模块原则
**核心要求：复用现有已实现的模块，不要重复造轮子**

##### 已实现模块清单

| 模块 | Crate | 核心接口 | 功能说明 |
|------|-------|----------|----------|
| **工具管理器** | `planned-agent-tool-manager` | `ToolRegistry` | 统一管理MCP、自定义、内置工具，支持自动路由、参数验证、分类查询 |
| **AI客户端** | `planned-agent-ai-openai` | `AiClient` trait | 支持OpenAI兼容API，流式响应，工具调用，重试逻辑 |
| **提示管理器** | `planned-agent-prompt-manager` | `PromptManager` trait | 模板化管理，变量替换，输出约束，JSON Schema验证 |
| **MCP客户端** | `planned-agent-mcp-rmcp` | `McpManager` | MCP服务器连接，工具同步，工具调用 |

##### 复用方式

1. **工具管理**：直接使用 `ToolRegistry`
   - 获取工具列表：`registry.get_all_tools()`
   - 调用工具：`registry.call_tool(name, args).await`
   - 按分类查询：`registry.get_tools_by_category(category)`
   - 搜索工具：`registry.search_tools(query)`

2. **AI调用**：直接使用 `AiClient` trait
   - 聊天完成：`client.chat_completion(request).await`
   - 流式响应：`client.chat_completion_stream(request).await`
   - 获取模型：`client.model_name()`

3. **提示管理**：直接使用 `PromptManager` trait
   - 加载模板：`manager.load_template(name).await`
   - 渲染模板：`manager.render(name, context).await`
   - 解析响应：`manager.parse_response::<T>(name, response).await`
   - 验证响应：`manager.validate_response(name, response).await`

##### 接口适配

Plan-and-Execute系统中的组件应该**组合**这些已实现模块，而不是重新实现：

```rust
// 正确示例：组合已实现模块
pub struct LlmCoarsePlanner {
    ai_client: Arc<dyn AiClient>,           // 复用AI客户端
    prompt_manager: Arc<dyn PromptManager>,  // 复用提示管理器
    tool_registry: Arc<ToolRegistry>,        // 复用工具管理器
}

// 错误示例：重新实现（禁止）
// pub struct MyAiClient { ... }  // 不要重新实现AI客户端
// pub struct MyToolManager { ... }  // 不要重新实现工具管理器
```

> 代码实现见 [LlmCoarsePlanner](file:///home/code/planned-agent/crates/planned-agent/src/planner/coarse/llm_planner.rs)

### 系统架构

#### 1. 核心组件

```
┌─────────────────────────────────────────────────────────────┐
│                    Plan-and-Execute System                   │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
│  │   Planner   │  │  Executor   │  │    Replanner        │ │
│  │  (规划器)   │  │  (执行器)   │  │  (重规划器)         │ │
│  └─────────────┘  └─────────────┘  └─────────────────────┘ │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
│  │   Tools     │  │   Memory    │  │  Supervisor         │ │
│  │  (工具管理) │  │  (记忆管理) │  │  (监督循环)         │ │
│  └─────────────┘  └─────────────┘  └─────────────────────┘ │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
│  │   MCP       │  │   Prompt    │  │  AI Client          │ │
│  │  (MCP客户端)│  │  (提示管理) │  │  (AI客户端)         │ │
│  └─────────────┘  └─────────────┘  └─────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

#### 2. 工作流程（完整交互图）

```
用户输入
    ↓
┌────────────────────────────────────────────┐
│              Supervisor                    │
│  - 初始化任务                              │
│  - 设置监控参数 (max_iterations, timeout)  │
│  - 跟踪整体执行进度                        │
└────────────────────────────────────────────┘
    ↓
┌────────────────────────────────────────────┐
│              Memory                        │
│  - 加载历史上下文                          │
│  - 恢复之前任务状态                        │
└────────────────────────────────────────────┘
    ↓
[1] 粗粒度计划生成 (Planner)
    ├─ Supervisor 调度 Planner
    ├─ Planner 分析用户需求
    ├─ 生成粗粒度步骤（只定义"做什么"）
    ├─ 定义步骤间依赖关系
    └─ 输出：CoarseGrainedPlan
    ↓
┌────────────────────────────────────────────┐
│              Memory                        │
│  - 存储生成的粗粒度计划                    │
│  - 记录计划版本历史                        │
└────────────────────────────────────────────┘
    ↓
[2] 详细步骤分析 (Planner)
    ├─ Supervisor 调度 Planner 继续分析
    ├─ 分析每个步骤的参数需求
    ├─ 探索可用工具（通过 ToolManager）
    ├─ 评估风险等级
    └─ 输出：DetailedPlanStep[]
    ↓
[3] 计划验证与确认 (Supervisor)
    ├─ Supervisor 检查步骤依赖关系
    ├─ Supervisor 评估执行风险
    ├─ 执行智能确认机制
    └─ 用户确认（复杂计划）
    ↓
[4] 逐步执行 (Executor)
    │   ↓
    │   ┌────────────────────────────────────────────┐
    │   │              Executor                      │
    │   │  - 从计划队列取出任务                      │
    │   │  - 管理执行上下文 ExecutionContext         │
    │   │  - 调用 ToolManager 执行工具               │
    │   │  - 收集执行结果                            │
    │   │  - 支持并行/串行执行                       │
    │   │  - 错误处理和重试                          │
    │   └────────────────────────────────────────────┘
    │   ↓
    │   ┌────────────────────────────────────────────┐
    │   │              Memory                        │
    │   │  - 存储执行结果 (#E1, #E2...)              │
    │   │  - 记录步骤执行日志                        │
    │   │  - 支持结果引用和依赖查找                  │
    │   └────────────────────────────────────────────┘
    │   ↓
    └─ 输出：StepResult[]
    ↓
[5] 动态重规划 (Replanner)
    │   ↓
    │   ┌────────────────────────────────────────────┐
    │   │              Replanner                     │
    │   │  - 接收执行结果 (PlanStepResult[])         │
    │   │  - 分析失败率是否超过阈值                  │
    │   │  - 检查是否返回意外结果                    │
    │   │  - 决定动作:                              │
    │   │    • Continue → 返回步骤4执行下一条        │
    │   │    • UpdatePlan → 返回步骤1重新规划        │
    │   │    • Abort → 终止任务                      │
    │   │    • RequestUserInput → 请求用户输入       │
    │   └────────────────────────────────────────────┘
    │   ↓
    │   ┌────────────────────────────────────────────┐
    │   │              Memory                        │
    │   │  - 更新上下文信息                          │
    │   │  - 记录重规划历史                          │
    │   │  - 保留失败信息供分析                      │
    │   └────────────────────────────────────────────┘
    │
    └─ 输出：UpdatedPlan 或 Continue/Abort
    ↓
┌────────────────────────────────────────────┐
│              Supervisor                    │
│  - 检查任务是否完成                        │
│  - 检查是否超时 (timeout_ms)               │
│  - 检查迭代次数 (max_iterations)           │
│  - 决定继续执行或终止                      │
└────────────────────────────────────────────┘
    ↓
[6] 结果汇总 (Supervisor + Memory)
    ├─ Supervisor 收集所有执行结果
    ├─ Memory 提供执行历史
    ├─ 生成最终报告
    └─ 输出：FinalResult
    ↓
┌────────────────────────────────────────────┐
│              Memory                        │
│  - 存储最终结果                            │
│  - 更新任务历史                            │
│  - 清理中间状态                            │
└────────────────────────────────────────────┘
```

#### 3. 组件职责总览

| 组件 | 主要职责 | 在工作流程中的位置 |
|------|----------|-------------------|
| **Supervisor** | 任务初始化、进度跟踪、超时控制、迭代限制、最终决策 | 全局协调者 |
| **Planner** | 粗粒度计划生成、详细步骤分析、计划验证 | 步骤1-3 |
| **Executor** | 步骤执行、工具调用、错误处理、重试 | 步骤4 |
| **Replanner** | 结果分析、重规划决策、计划更新 | 步骤5 |
| **Memory** | 上下文存储、结果缓存、历史记录 | 贯穿全程 |
| **ToolManager** | 工具探索、工具执行 | 步骤2、4 |

#### 4. 组件交互时序

```
用户 ──→ Supervisor ──→ Planner ──→ Executor ──→ Replanner ──→ Supervisor
          │                │              │              │          │
          ↓                ↓              ↓              ↓          ↓
        Memory          Memory          Memory        Memory     Memory

时序说明：
1. Supervisor 初始化任务，从 Memory 加载上下文
2. Supervisor 调用 Planner 生成计划，结果存入 Memory
3. Planner 生成详细步骤，ToolManager 参与工具探索
4. Executor 按依赖顺序执行步骤，结果存入 Memory
5. Replanner 分析结果，决定是否重规划
6. Supervisor 检查终止条件，循环或结束
```

> 代码实现见 [Planner模块结构](file:///home/code/planned-agent/crates/planned-agent/src/planner/mod.rs)

### 配置设计

#### 1. 计划器配置

```toml
[planner]
# 基本配置
temperature = 0.1
max_retries = 3
default_complexity = "simple"

# 两阶段计划生成配置
[planner.two_phase]
# 第一阶段：粗粒度计划生成
coarse_plan_temperature = 0.3  # 允许更多创造性
coarse_plan_max_steps = 10

# 第二阶段：步骤详细分析
step_analysis_temperature = 0.1  # 保证稳定性
step_analysis_timeout_ms = 10000

# 工具探索配置
[planner.two_phase.tool_exploration]
# 是否启用工具探索
enabled = true
# 探索策略（explore/conservative/aggressive）
strategy = "explore"
# 最大探索工具数
max_tools_to_explore = 5
# 探索超时时间
exploration_timeout_ms = 5000

# 结果引用配置
[planner.two_phase.result_references]
# 是否启用结果引用
enabled = true
# 引用语法（如 #E1、#E2）
syntax = "#E{number}"
# 最大引用深度
max_reference_depth = 5

# 模板配置
[planner.templates]
enabled = true
template_dir = "./templates"

# 智能确认配置
[planner.confirmation]
# 简单计划是否自动执行
auto_execute_simple = true
# 复杂度阈值（超过此值需确认）
complexity_threshold = "medium"
# 高风险步骤是否需要确认
high_risk_confirmation = true

# 监督器配置
[planner.supervisor]
max_iterations = 10
timeout_ms = 30000
retry_attempts = 3
fallback_strategy = "retry_with_different_tool"
progress_threshold = 0.1

# 版本管理配置
[planner.versioning]
enabled = true
max_versions = 10
auto_checkpoint = true
```

#### 2. 执行器配置

```toml
[executor]
# 基本配置
max_concurrent_steps = 3
step_timeout_ms = 30000
retry_attempts = 3

# 工具探索配置
[executor.tool_exploration]
# 是否启用工具探索
enabled = true
# 探索策略
strategy = "explore"
# 探索超时时间
exploration_timeout_ms = 5000
# 最大探索工具数
max_tools_to_explore = 5

# 错误处理配置
[executor.error_handling]
# 是否自动重试
auto_retry = true
# 最大重试次数
max_retries = 3
# 重试延迟
retry_delay_ms = 1000
# 是否回退到替代工具
fallback_to_alternative = true

# 并行执行配置
[executor.parallel]
# 是否启用并行执行
enabled = true
# 最大并行数
max_parallel = 3
# 是否等待所有并行任务完成
wait_for_all = true
```

#### 3. 重规划器配置

```toml
[replanner]
# 基本配置
enabled = true
# 重规划触发条件
trigger_on_failure = true
trigger_on_unexpected_result = true
trigger_on_timeout = true

# 重规划策略
[replanner.strategy]
# 重规划类型（conservative/moderate/aggressive）
type = "moderate"
# 最大重规划次数
max_replans = 3
# 重规划超时时间
timeout_ms = 10000

# 重规划触发阈值
[replanner.triggers]
# 失败率阈值（超过此值触发重规划）
failure_rate_threshold = 0.3
# 意外结果阈值
unexpected_result_threshold = 0.5
# 超时阈值
timeout_threshold = 0.2
```

## 核心类型设计

### 1. 计划类型

```rust
/// 粗粒度计划
pub struct CoarseGrainedPlan {
    pub id: String,
    pub title: String,
    pub description: String,
    pub steps: Vec<CoarseGrainedStep>,
    pub created_at: DateTime<Utc>,
    pub complexity: PlanComplexity,
    pub risk_level: RiskLevel,
}

/// 粗粒度步骤
pub struct CoarseGrainedStep {
    pub id: String,
    pub order: u32,
    pub intent: String,  // 意图描述，如："获取搜索结果"
    pub expected_output: String,  // 预期输出描述
    pub result_reference: String,  // 结果引用标识，如："#E1"
    pub dependencies: Vec<String>,  // 依赖的步骤结果引用列表
    pub data_requirements: Vec<DataRequirement>,
}

/// 数据需求
pub struct DataRequirement {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub source_hint: String,  // 来源提示，如："从搜索结果中提取"
}
```

> 代码实现见 [粗粒度计划类型定义](file:///home/code/planned-agent/crates/core/src/planner/coarse/coarse_types.rs)

### 2. 详细步骤类型

```rust
/// 详细计划步骤
pub struct DetailedPlanStep {
    pub id: String,
    pub coarse_step_id: String,
    pub intent: String,
    pub parameters: serde_json::Value,
    pub tool_exploration: ToolExploration,
    pub risk_assessment: RiskAssessment,
    pub execution_strategy: ExecutionStrategy,
}

/// 工具探索结果
pub struct ToolExploration {
    pub discovered_tools: Vec<DiscoveredTool>,
    pub recommended_tool: Option<DiscoveredTool>,
    pub exploration_notes: String,
}

/// 发现的工具
pub struct DiscoveredTool {
    pub name: String,
    pub description: String,
    pub relevance_score: f32,  // 0-1
    pub parameters_schema: serde_json::Value,
}

/// 风险评估
pub struct RiskAssessment {
    pub risk_level: RiskLevel,
    pub risk_factors: Vec<String>,
    pub mitigation_strategies: Vec<String>,
}

/// 执行策略
pub struct ExecutionStrategy {
    pub strategy_type: ExecutionStrategyType,
    pub fallback_options: Vec<String>,
    pub timeout_ms: u64,
    pub retry_count: u32,
}

/// 执行策略类型
pub enum ExecutionStrategyType {
    Direct,      // 直接执行
    Explore,     // 探索执行
    Fallback,    // 回退执行
    Parallel,    // 并行执行
}
```

> 代码实现见 [详细步骤类型定义](file:///home/code/planned-agent/crates/core/src/planner/detailed/detailed_types.rs)

### 3. 执行类型

```rust
/// 计划执行
pub struct PlanExecution {
    pub plan_id: String,
    pub status: PlanExecutionStatus,
    pub results: Vec<PlanStepResult>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub execution_history: Vec<ExecutionEvent>,
}

/// 计划步骤结果
pub struct PlanStepResult {
    pub step_id: String,
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub tool_used: String,
    pub exploration_log: Vec<ExplorationLogEntry>,
}

/// 执行事件
pub struct ExecutionEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: ExecutionEventType,
    pub step_id: String,
    pub details: String,
}

/// 执行事件类型
pub enum ExecutionEventType {
    StepStarted,
    StepCompleted,
    StepFailed,
    ToolCalled,
    ToolResult,
    Replanning,
    PlanUpdated,
}
```

> 代码实现见 [执行类型定义](file:///home/code/planned-agent/crates/core/src/planner/executor/executor_types.rs)

### 4. 重规划类型

```rust
/// 重规划请求
pub struct ReplanningRequest {
    pub original_plan: CoarseGrainedPlan,
    pub execution_results: Vec<PlanStepResult>,
    pub remaining_steps: Vec<CoarseGrainedStep>,
    pub user_goal: String,
    pub current_context: serde_json::Value,
}

/// 重规划响应
pub struct ReplanningResponse {
    pub action: ReplanningAction,
    pub updated_plan: Option<CoarseGrainedPlan>,
    pub updated_steps: Option<Vec<CoarseGrainedStep>>,
    pub reason: String,
}

/// 重规划动作
pub enum ReplanningAction {
    Continue,      // 继续执行原计划
    UpdatePlan,    // 更新计划
    Abort,         // 中止执行
    RequestUserInput,  // 请求用户输入
}
```

> 代码实现见 [重规划类型定义](file:///home/code/planned-agent/crates/core/src/planner/replanner/replanner_types.rs)

### 5. 验证类型

```rust
/// 计划验证结果
pub struct PlanValidationResult {
    pub valid: bool,
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
    pub risk_level: RiskLevel,
}

/// 验证错误
pub struct ValidationError {
    pub error_type: ValidationErrorType,
    pub message: String,
    pub step_id: Option<String>,
}

/// 验证错误类型
pub enum ValidationErrorType {
    CircularDependency,
    MissingDependency,
    InvalidStep,
    ToolNotFound,
    RiskTooHigh,
}

/// 验证警告
pub struct ValidationWarning {
    pub warning_type: ValidationWarningType,
    pub message: String,
    pub step_id: Option<String>,
}

/// 验证警告类型
pub enum ValidationWarningType {
    HighRisk,
    ComplexDependency,
    LongExecutionTime,
    ManySteps,
}
```

> 代码实现见 [验证类型定义](file:///home/code/planned-agent/crates/core/src/planner/validation/validation_types.rs)

## 实现阶段（分层实现）

### 阶段一：Core层接口定义（第1-2天）
**目标**：完善所有接口定义，确保模块间解耦

**任务**：
1. 定义详细步骤类型（DetailedPlanStep、ToolExploration等）
2. 定义重规划类型（ReplanningRequest、ReplanningResponse等）
3. 定义执行类型（PlanExecution、PlanStepResult等）
4. 定义错误类型（PlanSystemError等）
5. 定义事件类型（SystemEvent等）
6. 定义所有接口（trait）

### 阶段二：计划层模块实现（第3-6天）
**目标**：独立实现各计划模块，每个模块可单独测试

**任务**：
1. 实现粗粒度计划器（LlmCoarsePlanner）
2. 实现详细步骤分析器（DetailedStepAnalyzer）
3. 实现执行器（StepExecutor）
4. 实现重规划器（DynamicReplanner）

> 代码实现见：
> - [粗粒度计划器实现](file:///home/code/planned-agent/crates/planned-agent/src/planner/coarse/llm_planner.rs)
> - [详细步骤分析器实现](file:///home/code/planned-agent/crates/planned-agent/src/planner/detailed/analyzer.rs)
> - [工具探索器实现](file:///home/code/planned-agent/crates/planned-agent/src/planner/detailed/tool_explorer.rs)

### 阶段三：支撑模块实现（第7-8天）
**目标**：实现支撑模块，为应用层做准备

**任务**：
1. 实现记忆管理器（InMemoryMemory）
2. 实现计划验证器（PlanValidator）
3. 创建提示模板
4. 扩展配置系统

### 阶段四：应用层协调（第9-11天）
**目标**：实现协调器，组织各模块协同工作

**任务**：
1. 实现监督器（DefaultSupervisor）
2. 实现协调器（PlanOrchestrator）
3. 集成所有模块
4. 编写集成测试

### 阶段五：集成测试与优化（第12-13天）
**目标**：端到端测试，性能优化

**任务**：
1. 端到端测试
2. 性能测试
3. 错误处理优化
4. 文档完善

## 测试计划（分层测试）

### 1. 单元测试（计划层模块）
- 测试粗粒度计划器（LlmCoarsePlanner）
- 测试详细步骤分析器（DetailedStepAnalyzer）
- 测试执行器（StepExecutor）
- 测试重规划器（DynamicReplanner）
- 测试记忆管理器（InMemoryMemory）
- 测试计划验证器（PlanValidator）

**测试方式**：
- 每个模块独立测试
- Mock外部依赖（AI客户端、MCP客户端）
- 快速反馈

### 2. 集成测试（应用层协调）
- 测试监督器（DefaultSupervisor）
- 测试协调器（PlanOrchestrator）
- 测试模块间交互
- 验证接口契约

**测试方式**：
- 测试模块间通信
- 验证数据流正确性
- 发现集成问题

### 3. 端到端测试（完整流程）
- 测试简单计划执行
- 测试复杂计划执行
- 测试动态重规划
- 测试错误处理和恢复
- 性能测试

**测试方式**：
- 测试完整业务流程
- 验证系统功能
- 性能基准测试

## 风险评估

### 1. 技术风险
- **AI响应解析**：LLM响应可能不符合预期格式
- **工具探索**：探索过程可能超时或失败
- **重规划循环**：可能陷入无限重规划循环

### 2. 缓解措施
- **响应验证**：使用JSON Schema验证LLM响应
- **超时控制**：设置合理的超时时间
- **循环检测**：检测并防止无限重规划循环

### 3. 性能风险
- **Token消耗**：Plan-and-Execute模式可能消耗大量Token
- **执行延迟**：串行执行可能导致延迟

### 4. 优化策略
- **混合架构**：结合ReWOO模式减少LLM调用
- **并行执行**：支持独立步骤并行执行
- **缓存机制**：缓存计划和执行结果

## 配置管理

配置管理模块支持多配置格式，包括多个 AI 提供商和多个 MCP 服务器。通过 `config.rs` 加载配置文件，并提供默认配置选择。

### 配置结构

- **多AI提供商配置**：支持多个AI提供商
- **多MCP服务器配置**：支持多个MCP服务器

> 详细配置结构见 [配置管理设计文档](./planned-agent/config-management.md)

## 命令行接口

命令行接口模块提供交互式命令行或 API 服务器，用于接收用户输入并返回响应。支持流式输出和直接输出模式。

> 详细实现见 [命令行接口设计文档](./planned-agent/cli-interface.md)
