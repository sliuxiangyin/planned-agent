# ReAct 执行组件

## 1. 组件定位

ReAct 组件负责执行粗粒度计划中的单个步骤。它把一个步骤目标交给模型，通过“思考、行动、执行、观察”的循环动态选择工具，并根据工具结果判断该步骤是否完成。

当前实现位于 `planned-agent` crate，通用接口和类型位于 `core` crate：

- 实现：[DefaultReActAgent](../../crates/planned-agent/src/planner/react/default_react_agent.rs)
- 模块入口：[react/mod.rs](../../crates/planned-agent/src/planner/react/mod.rs)
- 抽象接口：[ReActAgent](../../crates/core/src/planner/react/react_trait.rs)
- 数据类型：[react_types.rs](../../crates/core/src/planner/react/react_types.rs)
- 思考提示词：[react_think.toml](../../prompts/planning/react_think.toml)
- 行动提示词：[react_act.toml](../../prompts/planning/react_act.toml)
- 观察提示词：[react_observe.toml](../../prompts/planning/react_observe.toml)

ReAct Agent 以粗粒度步骤为执行边界，不负责生成整个计划，也不负责跨步骤的重规划。

## 2. 组件边界

### 2.1 负责的工作

1. 分析当前粗粒度步骤、前序输出和执行历史。
2. 从工具注册表中读取工具描述。
3. 通过模型选择工具并生成参数。
4. 执行内置工具、MCP 工具或 `ai_process`。
5. 分析工具结果并判断当前步骤是否完成。
6. 保存每轮的思考、行动和观察记录。
7. 在达到完成条件、执行失败或超过迭代上限时返回结果。

### 2.2 不负责的工作

- 不生成粗粒度计划。
- 不改变粗粒度计划的步骤顺序。
- 不负责多个粗粒度步骤之间的调度。
- 不直接管理 AI 提供商或 MCP 连接生命周期。
- 不将工具定义直接传给模型的原生 `tools` 字段；当前实现将工具描述写入 Prompt 文本。

## 3. 依赖关系

`DefaultReActAgent` 组合以下依赖：

| 依赖 | 用途 |
| --- | --- |
| `AiClient` | 调用思考、行动、观察以及 `ai_process` 所需的模型请求 |
| `PromptManager` | 渲染三个 ReAct Prompt 并解析结构化响应 |
| `ToolRegistry` | 获取工具描述并路由实际工具调用 |
| `CoarseGrainedStep` | 提供当前步骤的目标和顺序信息 |
| `PlanContext` | 传递前序输出和后续步骤摘要 |

## 4. 核心数据类型

### 4.1 `Thought`

思考阶段的模型输出：

- `reasoning`：当前状态分析和推理过程。
- `plan`：下一步计划。
- `confidence`：`0.0` 到 `1.0` 的置信度。

### 4.2 `Action`

行动阶段的模型输出：

- `tool_name`：要调用的工具名称。
- `parameters`：工具参数，类型为 JSON `Value`。
- `reasoning`：选择该工具的理由。

### 4.3 `Observation`

工具执行后形成的观察：

- `output`：工具输出。
- `is_complete`：工具层面的完成标记；普通工具执行后默认是 `false`，最终完成由观察阶段判断。
- `error`：工具错误信息。
- `duration_ms`：本轮工具执行耗时。

### 4.4 `ReActStep`

一轮完整循环的历史记录，由 `Thought`、`Action` 和 `Observation` 组成。

### 4.5 `ReActExecutionResult`

最终返回值包含：

- `step_id`：当前粗粒度步骤 ID。
- `success`：该步骤是否成功完成。
- `output`：最终工具输出。
- `error`：失败原因。
- `history`：完整执行历史。
- `iterations`：使用的循环次数。
- `total_duration_ms`：总耗时。

## 5. 单步执行流程

`execute_coarse_step` 对当前步骤最多执行 `max_iterations` 轮：

```text
粗粒度步骤和上下文
        |
        v
      THINK  -> 生成 Thought
        |
        v
       ACT   -> 生成 Action
        |
        v
    EXECUTE  -> 调用工具并生成 Observation
        |
        v
     OBSERVE -> 判断是否完成
        |
   +----+----+
   |         |
完成       未完成
   |         |
返回结果   进入下一轮
```

### 5.1 THINK

`think` 会构建以下 Prompt 变量：

- `coarse_step`：当前步骤意图。
- `tools`：所有已注册工具的名称、描述和输入 Schema。
- `history`：之前循环的思考、行动和观察。
- `previous_outputs`：当前步骤之前的原始输出。
- `remaining_steps`：当前步骤之后的步骤摘要。

模型需要返回 `Thought` JSON。Prompt 明确要求模型优先复用前序输出；需要处理已有数据时通常选择 `ai_process`。

### 5.2 ACT

`act` 将以下信息交给模型：

- 当前思考中的 `reasoning` 和 `plan`。
- 可用工具描述。
- 前序步骤的原始输出。

模型必须返回包含 `tool_name`、`parameters` 和 `reasoning` 的 `Action` JSON。行动解析失败时，外层执行循环最多重试三次；三次都失败则当前步骤失败。

### 5.3 EXECUTE

工具执行分为两条路径：

#### 普通工具

1. 克隆工具名称和参数。
2. 在阻塞线程中调用 `ToolRegistry.call_tool`，避免持有读锁影响异步任务。
3. 将 `ToolResult.content` 包装为 `Observation`。
4. 工具返回错误时设置 `Observation.error`，并保持未完成状态。

#### `ai_process`

`ai_process` 由 ReAct Agent 特殊处理，不经过注册表中的占位执行器：

1. 从参数中读取 `data` 和 `instruction`。
2. 将数据和指令组成新的模型 Prompt。
3. 调用模型处理数据。
4. 如果响应是合法 JSON，则保存为 JSON；否则保存为字符串。
5. 返回未完成的 `Observation`，由后续 OBSERVE 阶段判断是否完成。

### 5.4 OBSERVE

`observe` 将当前步骤意图和工具结果再次发送给模型。模型返回：

```json
{
  "is_complete": true,
  "reasoning": "判断理由"
}
```

当 `is_complete` 为 `true` 时，ReAct Agent 将本轮工具输出作为当前步骤最终输出；否则继续下一轮循环。

如果工具执行已经产生错误，`observe` 不再调用模型，而是直接返回未完成结果。

## 6. 上下文传递

主程序在逐步执行计划时为每个步骤复制 `PlanContext`，并使用 `metadata` 传递数据：

| Metadata 键 | 内容 |
| --- | --- |
| `previous_outputs` | 已完成步骤的编号和原始输出数组 |
| `remaining_steps` | 当前步骤之后的 `CoarseGrainedStep` 数组 |

ReAct Agent 读取方式：

- `think` 将两类数据都写入思考 Prompt。
- `act` 将 `previous_outputs` 写入行动 Prompt。
- 后续步骤可以直接引用前序结果，而不必重复执行获取数据的工具。

当前主程序的协调逻辑位于 [main.rs](../../crates/planned-agent/src/main.rs) 的 `run_test_execute`。

## 7. Prompt 契约

### 7.1 `planning/react_think`

输入变量：

- `coarse_step`
- `tools`
- `history`
- `previous_outputs`
- `remaining_steps`

输出字段：

- `reasoning`
- `plan`
- `confidence`

### 7.2 `planning/react_act`

输入变量：

- `thought`
- `tools`
- `previous_outputs`

输出字段：

- `tool_name`
- `parameters`
- `reasoning`

当前行动 Prompt 要求必须选择工具，因此当思考阶段判断“已有数据已足够、无需再调用工具”时，模型仍可能生成一个工具行动。项目目前没有专门的完成或跳过 Action 类型。

### 7.3 `planning/react_observe`

输入变量：

- `coarse_step`
- `tool_result`

输出字段：

- `is_complete`
- `reasoning`

三个 Prompt 的结构化响应都由 `PromptManager.parse_response` 解析。Prompt 管理器可以清除 Markdown JSON 代码块和前后说明，但格式错误的 JSON 仍会导致解析失败。

## 8. 配置

`ReActAgentConfig` 定义以下配置项：

| 配置项 | 默认值 | 说明 |
| --- | ---: | --- |
| `max_iterations` | `10` | 单个粗粒度步骤允许的最大循环次数 |
| `step_timeout_ms` | `30000` | 单步超时配置字段 |
| `enable_chain_of_thought` | `true` | 是否启用思考链配置字段 |
| `max_retries` | `3` | 重试配置字段 |
| `retry_delay_ms` | `1000` | 重试延迟配置字段 |

测试执行入口使用 `max_iterations = 5`，其他字段与默认配置一致。

## 9. 使用示例

```rust
let react_agent = DefaultReActAgent::new(
    ai_client,
    prompt_manager,
    tool_registry,
    ReActAgentConfig::default(),
);

let result = react_agent
    .execute_coarse_step(&coarse_step, &context)
    .await?;

if result.success {
    println!("最终输出: {}", result.output);
} else if let Some(error) = result.error {
    eprintln!("步骤执行失败: {}", error);
}
```

## 10. 错误处理和终止条件

### 10.1 思考失败

思考 Prompt 渲染、模型调用或响应解析失败时，当前步骤立即失败，不进入行动阶段。

### 10.2 行动失败

行动响应解析失败时最多重试三次。重试仍失败时，返回失败的 `ReActExecutionResult`。

### 10.3 工具失败

工具错误会被包装为观察结果。观察阶段会将其判定为未完成，执行循环可能继续尝试其他行动。

### 10.4 观察失败

观察 Prompt 渲染、模型调用或响应解析失败时，当前轮被记录为未完成；如果仍有迭代预算，则进入下一轮。

### 10.5 超过迭代上限

循环达到 `max_iterations` 仍未完成时，返回失败结果，错误信息包含最大迭代次数。

## 11. 执行历史

每一轮都会保存：

```text
第 N 轮
  思考：Thought.reasoning
  行动：Action.tool_name(Action.parameters)
  观察：Observation.output
```

历史既用于最终诊断，也会在下一轮 THINK 阶段重新提供给模型。最终结果只返回最后一次成功观察对应的工具输出，不会自动汇总所有轮次结果。

## 12. 当前限制与排查建议

- `step_timeout_ms` 当前只是配置字段，`execute_coarse_step` 没有实际应用超时控制。
- `enable_chain_of_thought` 当前没有改变执行分支。
- 行动重试次数在执行循环中固定为三次，尚未读取 `max_retries`。
- `retry_delay_ms` 当前没有用于等待重试。
- `is_complete` 辅助方法存在，但主循环使用的是 `observe` 返回的 `ObserveResult`。
- `ai_process` 每次执行后都将工具观察标记为未完成，完成判断依赖额外的 OBSERVE 模型调用。
- 行动 Prompt 要求必须选择工具，没有显式的“完成当前步骤”协议；当模型判断无需操作时，可能选择无关工具。
- 模型返回未转义引号、额外说明或不完整 JSON 时，`PromptManager.parse_response` 会失败并触发行动重试。
- 工具名称必须与 `ToolRegistry` 中的注册名称一致，否则会得到工具调用错误。

排查时建议按以下顺序查看：

1. 检查 `Thought` 中是否正确理解了当前目标和前序输出。
2. 检查 `Action.tool_name` 是否为已注册工具。
3. 检查 `Action.parameters` 是否符合工具输入 Schema。
4. 检查工具原始输出和 `Observation.error`。
5. 检查 `react_observe` 的 `is_complete` 判断及理由。
6. 检查执行历史，确认是否发生了重复调用或无效重试。

## 13. 测试建议

ReAct 组件适合使用 Mock `AiClient`、Mock `PromptManager` 和最小化 `ToolRegistry` 进行测试，至少覆盖：

- 一轮工具调用后完成。
- 工具返回错误后重试。
- 行动 JSON 解析失败后的重试。
- `ai_process` 处理 JSON 和纯文本输出。
- 前序输出和后续步骤摘要能够进入 Prompt。
- 达到最大迭代次数后返回失败。
