# planned-agent-core 开发须知

> 面向后续开发的 LLM / 贡献者。**改这个 crate 之前先读这份**，可以省掉读遍全部源码。
> 内容会随代码演进，若与代码冲突，**以代码为准并顺手更新本文**。

## 0. 一句话

`planned-agent-core` 是 **Agent 核心库**：**能力抽象与领域模型的家**。
绝大多数情况下，你在这里**定义 trait 与数据类型**，而把**实体实现放到下游 crate**。

```rust
// crates/core/src/tool_registry/mod.rs:5-9（原文）
// 本模块定义 `ToolRegistry` 需要的全部抽象类型（`ToolSource`、`ToolCategory`、
// `ToolExecutor`、`BuiltinToolProvider`、`McpManagerTrait`），
// 具体实现在下游 crate（`tool-manager`、`mcp-rmcp` 等）中完成。
// 这种"抽象在 core，实现在上层"的模式避免了 crate 之间相互依赖。
```

## 1. 定位与边界

**定位**（`src/lib.rs:1-3` 原文）：`Agent 核心库，提供 AI 交互、规划、工具执行等抽象接口。`

| 是 | 不是 |
|---|---|
| 能力 **trait** 的定义处（`AiClient` / `ToolExecutor` / `McpClient` …） | 那些 trait 的**具体实现**（在下游 crate） |
| 跨 crate **共用的数据模型**（`Message` / `Plan` / `ChatEvent` …） | 运行时**可变状态**（会话 / 缓存 / 连接池） |
| 宿主事实的探测（`host/`） | 业务 / UI / 落库逻辑 |

**依赖方向（硬约束）**

- core **不依赖任何同仓 crate**（`Cargo.toml:6-16` 只有外部依赖 + `which`）。新增依赖前先想清楚是否该放到下游。
- 依赖 core 的 crate：`agent-gui`、`ai-manager`、`ai-openai`、`mcp-rmcp`、`planned-agent`、`prompt-manager`、`tool-manager`。**core 改动的破坏面很广**，改公开 API 前先 grep 全仓用法。
- **不是纯粹只有抽象**：`planner/` 里有规划器的**实现**（Coarse / ReAct / RePlanner，`planner/mod.rs:3-4`）。判断某段代码该不该放 core，用的不是「抽象还是实现」，而是「**是否被多个下游共用**」。

## 2. 目录地图

所有模块都是**目录 + `mod.rs`** 形态（没有裸 `.rs` 模块文件）。

| 模块 | 放什么 | 对外主要类型（file:line） |
|---|---|---|
| `ai/` | AI 交互抽象 + 消息数据模型 | trait `AiClient`(`ai/traits.rs:7`)、`ChatCompletionStream`(`:25`)；`Message`/`MessageContent`/`MessageRole`/`ToolCall`/`ToolType`(`ai/mod.rs:13-15` 重导出)；`AiProviderConfig`/`ThinkingConfig` 走 `ai::config` |
| `prompt/` | Prompt 管理抽象 | trait `PromptManager`(`prompt/traits.rs:114`)；`PromptTemplate`/`PromptContext`/`PromptInfo`/`OutputSchema`/`OutputFormat`(`prompt/traits.rs`) |
| `tool_registry/` | 工具注册与执行的**抽象** | trait `ToolExecutor`(`traits.rs:11`)、`BuiltinToolProvider`(`:35`)、`McpManagerTrait`(`:52`)；enum `ToolSource`(`types.rs:5`)、`ToolCategory`(`:18`) |
| `planner/` | 规划器实现 + Plan 领域模型 | `PlanContext`/`Plan`/`PlanStep`/`PlanStepStatus`(`types.rs`)；trait `CoarsePlanner`(`coarse/coarse_planner.rs:10`)、`ReActAgent`(`react/react_trait.rs:12`)；子目录 `coarse/` `react/` `replanner/` `trace/` `validation/` |
| `mcp/` | MCP **抽象**（实现在 `crates/mcp-rmcp`） | trait `McpClient`(`traits.rs:8`)；`Tool`/`ToolResult`/`McpServerConfig`/`ConnectionStatus`/`ConnectionError`(`types.rs`) |
| `errors/` | Agent 错误类型 | `PlanSystemError`(`error_types.rs:6`)、`ErrorRecoveryStrategy`(`:70`)、`ErrorContext`(`:90`)（**未**顶层重导出） |
| `events/` | 执行事件系统 | `ChatEvent`(`chat_event.rs:29`)、`UIOption`/`UIQuestion`(`ui_action.rs`)；`SystemEvent`/`UserInteractionType`(`event_types.rs`) **未**重导出 |
| `host/` | **宿主事实**：启动时探测一次、之后只读 | `RuntimeEnvironment`(`environment.rs:51`)、`ExecutableProbe`、`probe_executables`、`DEFAULT_PROBE_NAMES` |

## 3. 约定（硬规则）

**① 异步 trait**：`#[async_trait]` + `: Send + Sync` + 返回 `anyhow::Result<_>`。
先例（共 6 个）：`AiClient`(`ai/traits.rs:6-7`)、`PromptManager`、`ToolExecutor`、`McpManagerTrait`、`McpClient`、`CoarsePlanner`、`ReActAgent`。
唯一的同步 trait 是 `BuiltinToolProvider`(`tool_registry/traits.rs:35`)——它没有任何 IO / 可变方法。

**② 错误处理**：默认用 `anyhow::Result`。
只有在需要**程序化判断「可重试 / 恢复策略」**时才用结构化的 `errors::PlanSystemError`（`thiserror` + serde，含 `is_retryable()` / `suggested_recovery_strategy()`）。**不要**为普通失败新建错误枚举。

**③ 测试**：内联 `#[cfg(test)] mod tests`（如 `host/environment.rs` 末尾、`events/chat_event.rs:113`）。
**不要**在 core 里建公共 testing 模块 / 测试桩——真实的桩（`ScriptedAiClient` 等）在下游 `planned-agent`（`src/flexible/testing.rs` 先例）。

**④ `mod.rs` 只做两件事**：`pub mod` 声明 + `pub use` 重导出常用类型。
**内部类型不做 core 顶层 re-export**，让使用者走完整路径（`planner/mod.rs:13-15` 先例）：

```rust
// 注意：子模块内部类型不导出到 core 顶层
// 如需使用，通过完整路径访问：
//   planned_agent_core::planner::coarse::CoarsePlan
```

**⑤ 数据类型的 derive 习惯**：`#[derive(Debug, Clone, Serialize, Deserialize)]`；`Default` 手写并**注释缺省语义**（如 `coarse/coarse_types.rs:31-36`）。

**⑥ 依赖最小化**：core 的依赖树是所有下游的基础，加依赖等于给全仓加负担。

## 4. 新的东西放哪？

| 你想加的东西 | 放哪 |
|---|---|
| 一个新的**能力抽象**（trait） | 对应模块的 `traits.rs`；**实现放下游 crate** |
| 领域**数据模型** | 对应模块的 `types.rs` / `*_types.rs` |
| **启动时探测一次、之后只读**的本机事实（版本号、路径、能力） | `host/`（准入门槛见 `host/mod.rs:3-12`） |
| 运行时**可变**状态（会话 / 缓存 / 连接池） | **不要放 core** → 下游 crate |
| 与 AI / 工具 / 规划领域相关的抽象 | 回 `ai/` / `tool_registry/` / `planner/`（`host/mod.rs:12` 也是这么写的） |
| 事件 / UI 交互数据 | `events/` |

`host/` 的准入门槛（`host/mod.rs:3-12`）**必须全部满足**：① 启动时探测 / 读取一次，之后只读；② 与 agent 领域逻辑无关；③ 不需要调用方传参。

## 5. 改完怎么验证

```powershell
cargo test  -p planned-agent-core      # core 单测
cargo check -p planned-agent           # 确认最主要的下游不破
```

core 是多个 crate 的依赖，改公开 API 后建议再 `cargo check --workspace`。

## 6. 已知出入 / 待清理

- `mcp/mod.rs:1` 的文档写「Model Context Protocol **实现**」，但 core 实际只放 `McpClient` trait 与数据模型，真实现在 `crates/mcp-rmcp` —— 措辞与「抽象在 core」的约定不符，看到时顺手修。
- **注释里的示例类型名已过时**，别照抄：`errors/mod.rs:9` 举例 `errors::error_types::AgentError`（实际类型是 `PlanSystemError`）；`events/mod.rs:14` 举例 `events::event_types::ExecutionEvent`（实际是 `SystemEvent`）。**以类型定义为准。**
- `docs/core.md` 是**早期**设计稿，已与代码脱节：它列出的 `crates/core/src/types.rs`、`factory/` 目录**现在都不存在**。读它时只当历史背景，**不要照抄**。
