# 粗粒度计划器组件

## 1. 组件定位

粗粒度计划器负责把用户的自然语言需求拆分成一组有顺序、可引用、可交给执行层处理的步骤。它只描述每一步要完成的目标，不负责决定具体工具调用参数或执行细节。

当前实现位于 `planned-agent` crate，核心接口和数据类型位于 `core` crate：

- 实现：[LlmCoarsePlanner](../../crates/planned-agent/src/planner/coarse/llm_planner.rs)
- 模块入口：[coarse/mod.rs](../../crates/planned-agent/src/planner/coarse/mod.rs)
- 抽象接口：[CoarsePlanner](../../crates/core/src/planner/coarse/coarse_planner.rs)
- 计划类型：[coarse_types.rs](../../crates/core/src/planner/coarse/coarse_types.rs)
- 计划提示词：[coarse_plan.toml](../../prompts/planning/coarse_plan.toml)

## 2. 组件边界

### 2.1 负责的工作

1. 接收用户需求和 `PlanContext`。
2. 生成包含步骤、依赖、结果引用、复杂度和风险等级的 `CoarseGrainedPlan`。
3. 为步骤推荐工具分类。
4. 对计划执行基础合法性检查。
5. 对步骤是否符合原子动作约束进行启发式检查，并记录警告。

### 2.2 不负责的工作

- 不直接调用 MCP 或内置工具。
- 不生成具体工具参数。
- 不执行计划步骤。
- 不负责动态重规划。
- 不会因为原子动作检查产生警告而拒绝计划；当前实现只记录调试日志。

## 3. 依赖关系

`LlmCoarsePlanner` 通过组合已有组件完成工作：

| 依赖 | 用途 |
| --- | --- |
| `AiClient` | 调用兼容 OpenAI 接口的模型生成计划 |
| `PromptManager` | 渲染 `planning/coarse_plan` 并解析 JSON 响应 |
| `PlanContext` | 提供历史对话和计划上下文 |
| `ToolCategory` | 提供可推荐的工具分类及分类说明 |

计划器本身不持有 `ToolRegistry`，因此它只推荐工具分类，不枚举或验证某个具体工具是否可用。

## 4. 核心数据结构

### 4.1 `CoarseGrainedPlan`

计划包含以下字段：

| 字段 | 说明 |
| --- | --- |
| `id` | 计划唯一标识 |
| `title` | 计划标题 |
| `description` | 计划整体说明 |
| `steps` | 按顺序排列的粗粒度步骤 |
| `created_at` | 创建时间，默认使用当前 UTC 时间 |
| `complexity` | `simple`、`medium` 或 `complex` |
| `risk_level` | `low`、`medium`、`high` 或 `critical` |

### 4.2 `CoarseGrainedStep`

每个步骤包含：

| 字段 | 说明 |
| --- | --- |
| `id` | 步骤 ID |
| `order` | 步骤顺序 |
| `intent` | 要完成的动作，例如“读取配置文件” |
| `expected_output` | 预期输出描述 |
| `result_reference` | 结果引用，例如 `#E1` |
| `dependencies` | 依赖的结果引用列表 |
| `data_requirements` | 执行该步骤所需的数据 |
| `recommended_tool_categories` | 可选的工具分类列表 |

### 4.3 `DataRequirement`

数据需求用于描述步骤需要哪些输入数据：

- `name`：数据名称。
- `description`：数据用途或内容说明。
- `required`：是否为必需数据。
- `source_hint`：数据来源提示，例如“从搜索结果中提取”。

## 5. 生成流程

`generate_coarse_plan` 的处理顺序如下：

1. **构建 Prompt 上下文**
   - `user_input`：用户原始需求。
   - `context`：历史对话；没有历史时使用“无历史上下文”。
   - `available_categories`：`ToolCategory::all()` 返回的全部工具分类及描述。
2. **渲染提示词**
   - 使用 `PromptManager.render("planning/coarse_plan", ...)`。
3. **调用模型**
   - 创建单条用户消息的 `ChatCompletionRequest`。
   - 温度固定为 `0.3`。
   - 最大输出 token 为 `2000`。
   - 不发送工具定义，工具参数不属于粗粒度规划阶段。
4. **提取模型文本**
   - 使用第一个 choice 的文本内容。
   - 没有 choice、没有文本内容或响应类型不支持时返回错误。
5. **解析计划**
   - 使用 `PromptManager.parse_response` 将 JSON 转换为 `CoarseGrainedPlan`。
6. **原子动作检查**
   - 检查步骤意图是否包含“并”“和”“然后”等连接词。
   - 通过关键词数量粗略统计动词数量。
   - 发现问题时输出调试级别警告，但仍返回计划。

## 6. Prompt 契约

计划模板要求模型只返回 JSON，并要求计划包含：

- 计划 ID、标题、描述。
- 步骤列表。
- 每个步骤的 `id`、`order`、`intent`、`expected_output`、`result_reference`。
- `dependencies`、`data_requirements` 和 `recommended_tool_categories`。
- `complexity` 和 `risk_level`。

模板强调以下拆分原则：

1. 一个步骤只包含一个原子操作。
2. “提取并排序”这类组合动作应拆成多个步骤。
3. 从命令输出中提取数据、排序、过滤和取前 N 项应分别建模。
4. 后续步骤通过结果引用依赖前序步骤结果。

## 7. 计划验证

`validate_coarse_plan` 提供独立的基础验证能力：

- 计划不能没有步骤。
- 步骤 ID 必须唯一。
- 结果引用必须唯一。
- 依赖引用不存在时产生警告。

返回值为 `CoarsePlanValidationResult`，包含：

- `valid`：是否存在阻断性错误。
- `errors`：阻断性错误列表。
- `warnings`：非阻断性警告列表。

需要注意，`generate_coarse_plan` 当前只调用原子动作检查，不会自动调用 `validate_coarse_plan`。调用方如需严格验证，应在生成计划后显式调用验证接口。

## 8. 与执行层的衔接

主程序中的 `run_test_execute` 负责协调粗粒度计划器和 ReAct Agent：

1. 创建 `LlmCoarsePlanner`。
2. 生成 `CoarseGrainedPlan`。
3. 逐个读取 `plan.steps`。
4. 将当前步骤传给 ReAct Agent 执行。
5. 将已完成步骤的输出写入 `PlanContext.metadata.previous_outputs`。
6. 将后续步骤摘要写入 `PlanContext.metadata.remaining_steps`。

因此，粗粒度计划器输出的 `intent` 是执行层的目标描述，`dependencies` 和 `result_reference` 是计划层的依赖信息，而具体的工具选择由 ReAct Agent 在执行时决定。

## 9. 使用示例

下面的示例展示组件的构造方式和核心调用方式：

```rust
let planner = LlmCoarsePlanner::new(
    ai_client,
    prompt_manager,
);

let plan = planner
    .generate_coarse_plan(user_input, &context)
    .await?;

let validation = planner.validate_coarse_plan(&plan).await?;
if !validation.valid {
    // 根据 validation.errors 终止或请求重新规划
}
```

## 10. 测试与扩展

实现文件中包含基于 Mock AI 客户端和 Mock PromptManager 的单元测试，覆盖计划响应的解析路径。

后续扩展可以集中在以下方向：

- 将原子动作检查从启发式关键词升级为结构化校验。
- 让验证失败能够阻止计划进入执行阶段。
- 根据实际 `ToolRegistry` 内容验证推荐分类是否可用。
- 增加对循环依赖、顺序错误和缺失结果引用的严格检查。
- 为模型请求增加可配置的温度、最大 token 和重试策略。

## 11. 当前限制

- 计划生成依赖模型输出符合 JSON 契约。
- `validate_atomic_steps` 只做简单中文关键词检查，可能产生误报或漏报。
- `validate_coarse_plan` 对缺失依赖只产生警告，不会将其视为错误。
- 计划器没有内置重试和降级逻辑，模型调用或响应解析失败会直接返回错误。
- 文档中的详细步骤分析器、执行器和监督器不属于当前 `planned-agent` crate 的已实现组件。
