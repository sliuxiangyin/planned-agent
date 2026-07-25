# 主程序 (`crates/planned-agent`)

整合所有 crate，提供命令行入口、交互模式以及 Plan-and-Execute 测试流程，是当前已实现计划引擎的实际承载者。

## 目录结构

```text
crates/planned-agent/
├── Cargo.toml
└── src/
    ├── main.rs        # 入口点、命令行分发、Plan-and-Execute 测试流程
    ├── config.rs      # 配置加载与默认值（AI 列表、MCP 服务器列表、Prompt 配置）
    ├── agent.rs       # 通用 Agent：AI/MCP/Prompt 初始化和单轮对话
    ├── cli.rs         # 基于 clap 的命令行参数与子命令
    └── planner/
        ├── mod.rs
        ├── coarse/
        │   ├── mod.rs
        │   └── llm_planner.rs    # LlmCoarsePlanner 粗粒度计划器
        └── react/
            ├── mod.rs
            └── default_react_agent.rs  # DefaultReActAgent 步骤执行器
```

## 模块职责

| 文件 | 主要职责 |
| --- | --- |
| `main.rs` | 初始化日志、加载配置、构建 `Agent`、分发命令行子命令；提供 `run_test_execute` 把粗粒度计划器和 ReAct Agent 串起来执行 |
| `agent.rs` | 提供 `Agent` 结构：注册内置工具、初始化 AI 客户端、连接 MCP、初始化 Prompt 管理器、单轮对话（含工具调用） |
| `config.rs` | `AppConfig` 加载 `config.toml`，并提供默认配置 |
| `cli.rs` | `Cli` 结构与 `Commands` 枚举（如 `TestExecute`） |
| `planner/coarse/llm_planner.rs` | `LlmCoarsePlanner` 实现，把用户输入拆分成 `CoarseGrainedPlan` |
| `planner/react/default_react_agent.rs` | `DefaultReActAgent` 实现，按 Think→Act→Execute→Observe 循环执行单个步骤 |

## 当前运行流程

### 单轮交互流程（`Agent::process_input`）

`Agent` 是与现有 AI 客户端、MCP 服务器和工具注册表对接的入口：

1. 通过 `AiManager` 获取 AI 客户端。
2. 从 `ToolRegistry` 取出全部工具定义，转换为 OpenAI `ToolDefinition`。
3. 构造 `ChatCompletionRequest` 并调用 `chat_completion`。
4. 如果响应中包含 `tool_calls`，逐个调用 `ToolRegistry::call_tool`（由 `tool-manager` 内部按 MCP/自定义/内置路由）。
5. 把工具结果拼成 `Tool ... result: ...` 后再次请求模型，获取最终回复。

`process_input_stream` 走相同路径，但使用 `chat_completion_stream` 收集流式响应。

### Plan-and-Execute 测试流程（`run_test_execute`）

由 `Commands::TestExecute { input }` 触发，是当前唯一串联计划生成与执行的主流程：

1. 从 `Agent` 获取默认 AI 客户端、`Arc<ToolRegistry>` 和 `Arc<FilePromptManager>`。
2. 构造 `LlmCoarsePlanner` 并调用 `generate_coarse_plan` 得到 `CoarseGrainedPlan`。
3. 构造 `ReActAgentConfig`（`max_iterations=5`、`step_timeout_ms=30000`、`max_retries=3`、`retry_delay_ms=1000`）。
4. 构造 `DefaultReActAgent`。
5. 遍历 `plan.steps`，对每个粗粒度步骤：
   - 复制基础 `PlanContext`，写入 `metadata.previous_outputs`（前序步骤的原始 `output`）和 `metadata.remaining_steps`（当前步骤之后的步骤摘要）。
   - 调用 `execute_coarse_step`。
   - 打印迭代次数、耗时、输出摘要、Think/Act/Observe 历史。
   - 把成功的输出追加到 `previous_outputs`，供后续步骤使用。
6. 打印 “测试完成”。

> 详细组件说明见：
> - [粗粒度计划器](./planned-agent/coarse-planner.md)
> - [ReAct 执行组件](./planned-agent/react-agent.md)

### 交互模式（`run_interactive_mode`）

`main.rs` 中的 `run_interactive_mode` 提供交互式 CLI，支持以下内建命令：

| 命令 | 作用 |
| --- | --- |
| `status` | 打印 `ToolRegistry` 状态（总数、启用、MCP/自定义/内置计数） |
| `tools` | 列出可用工具及其分类描述 |
| `providers` | 列出已配置的 AI 提供商 |
| `servers` | 列出已连接的 MCP 服务器 |
| `prompts` | 列出 `PromptManager` 中可用的提示模板 |
| `test-prompt` | 渲染 `analysis/extract_info` 并演示校验和解析 |
| `test-coarse` | 调用 `LlmCoarsePlanner` 演示计划生成 |
| `quit`/`exit` | 退出交互模式 |

其他输入会按 `cli.stream` 走 `process_input_stream` 或 `process_input`。

## 已复用模块

主程序不重复实现 AI、Prompt、Tool、MCP 能力，全部复用：

| 复用模块 | 主要 API |
| --- | --- |
| `planned-agent-ai-manager` | `AiManager::from_config`、`default()`、`get(name)`、`provider_names()` |
| `planned-agent-ai-openai` | `AiClient::chat_completion`、`chat_completion_stream`、`model_name()` |
| `planned-agent-mcp-rmcp` | `McpManager::connect_all`、`get_connection_status` |
| `planned-agent-prompt-manager` | `FilePromptManager::new/initialize`、`render`、`parse_response`、`validate_response`、`list_prompts` |
| `planned-agent-tool-manager` | `ToolRegistry::new`、`register_builtin_provider`、`call_tool`、`get_all_tools`、`get_metadata`、`set_mcp_manager` |
| `planned-agent-core` | `CoarsePlanner`、`ReActAgent`、`ReActAgentConfig`、`PlanContext`、`PlanStepResult` 等 |

## 计划引擎设计

主程序承载计划引擎的 **最小可运行链路**：

- 粗粒度计划器负责把用户需求拆分为若干步骤（描述 “做什么”）。
- ReAct Agent 按粗粒度步骤动态选择具体工具并执行（决定 “怎么做”）。

### 设计原则

1. **复用已有组件**：计划引擎只组合 `AiClient`、`PromptManager`、`ToolRegistry`，不直接调用 HTTP、MCP 或模型。
2. **上下文单向流动**：计划步骤的执行结果通过 `PlanContext.metadata.previous_outputs` 显式传递给后续步骤。
3. **后续步骤可见**：将剩余步骤的摘要写入 `PlanContext.metadata.remaining_steps`，帮助 ReAct 在执行时考虑全局进度。
4. **`ai_process` 是特殊工具**：当需要在前序输出基础上做二次加工时，由 ReAct Agent 内部特殊处理，不走注册表中的占位执行器。

### 最小链路与未来扩展

| 组件 | 状态 | 位置 |
| --- | --- | --- |
| 粗粒度计划器 | 已实现 | [LlmCoarsePlanner](../crates/planned-agent/src/planner/coarse/llm_planner.rs) / [组件文档](./planned-agent/coarse-planner.md) |
| ReAct 执行组件 | 已实现 | [DefaultReActAgent](../crates/planned-agent/src/planner/react/default_react_agent.rs) / [组件文档](./planned-agent/react-agent.md) |
| 详细步骤分析器 | 未实现 | `crates/core/src/planner/detailed/`（目录为空） |
| 统一执行器 | 未实现 | `crates/core/src/planner/executor/`（目录为空） |
| 监督器 / 协调器 | 未实现 | 当前未在 `planned-agent` crate 中提供 |
| 完整动态重规划 | 未实现 | `ReplanningRequest`/`ReplanningResponse` 已定义类型，但未提供完整协调实现 |

> `crates/core/src/planner/replanner` 和 `crates/core/src/planner/validation` 中仅提供数据类型（`ReplanningRequest`、`ReplanningResponse`、`ReplanningAction`、`PlanValidationResult`、`ValidationError`、`ValidationWarning` 等），调用方不能将其误认为已实现的协调组件。

### ReAct Action 的已知限制

`DefaultReActAgent` 当前没有显式的 “完成” 或 “无操作” Action：

- `react_act` Prompt 强制要求模型选择一个工具；
- 如果模型判断前序输出已经足够，仍然会尝试生成一次工具调用；
- `PromptManager` 的 JSON 清洗只会去掉 Markdown 代码块和首尾多余文本，无法修复字符串内未转义的双引号；
- 因此 Action JSON 解析失败时会触发外层固定三次重试。

详见 [ReAct 执行组件](./planned-agent/react-agent.md) 的 “当前限制” 章节。

## 配置管理

`config.rs` 加载 `config.toml`，结构由 `AppConfig` 描述，包含：

- `ai_providers: Vec<AiProviderConfig>`：AI 客户端列表（OpenAI 兼容）。
- `mcp_servers: Vec<McpServerConfig>`：MCP 服务器列表。
- `prompt_manager: PromptManagerConfig`：提示目录等配置。

如果配置文件不存在，会回退到 `AppConfig::default_config()` 并打印 “Using default configuration”。

## 命令行接口

`cli.rs` 使用 `clap` 的 derive 模式定义参数，至少支持：

- `--stream`：开启流式响应。
- `--interactive`：进入交互模式。
- 子命令 `TestExecute --input <INPUT>`：执行 `run_test_execute`。

`Cli::merge_with_config` 会把命令行参数与配置合并，命令行覆盖配置文件。

## 实现状态总结

| 模块 | 状态 |
| --- | --- |
| 工作空间、依赖、AI/MCP/Prompt/Tool 各 crate | 已实现 |
| `AiManager` 多提供商路由 | 已实现 |
| `Agent` 单轮对话（含工具调用） | 已实现 |
| 交互 CLI（`status/tools/providers/servers/prompts/test-prompt/test-coarse`） | 已实现 |
| `LlmCoarsePlanner` 粗粒度计划生成 | 已实现 |
| `DefaultReActAgent` ReAct 四阶段循环 | 已实现 |
| Plan-and-Execute 测试入口（`run_test_execute`） | 已实现 |
| 详细步骤分析器 | 未实现 |
| 统一执行器 / Supervisor / PlanOrchestrator | 未实现 |
| 完整动态重规划流程 | 未实现 |
| 持久化、Web API、插件系统、热重载 | 未实现 |

## 测试与验证

当前已包含：

- `crates/planned-agent/src/planner/coarse/llm_planner.rs` 内基于 Mock AI 客户端和 Mock PromptManager 的单元测试，覆盖响应解析路径。
- `run_test_execute` 作为端到端集成入口，需要有效的 AI 配置和 MCP 服务器才能跑通。
- 交互模式中的 `test-prompt`、`test-coarse` 可手动验证 Prompt 渲染、解析和粗粒度计划生成。

后续可补充：

- ReAct 阶段的单元测试（需 Mock `AiClient`、`PromptManager`、`ToolRegistry`）。
- 步骤间上下文传递的端到端测试（验证 `previous_outputs` 的实际拼接效果）。
- 重规划与验证类型的契约测试。

## 风险与注意事项

1. **响应解析失败**：当前 `react_act` 阶段如果模型返回未转义的引号，`PromptManager.parse_response` 会失败，触发外层重试；尚未修复根本原因。
2. **超时配置未实际接入**：`step_timeout_ms`、`max_retries`、`retry_delay_ms` 当前未真正驱动 ReAct 控制逻辑。
3. **动态重规划尚未闭环**：主程序目前只能串行执行粗粒度计划，遇到失败只能中断，无法替换后续步骤。
4. **MCP 连接**：连接失败时仅记录错误并继续运行，可能导致部分工具不可用但流程仍可启动。