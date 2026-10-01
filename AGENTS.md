# planned-agent — 仓库导航

> 面向后续开发的 LLM / 新贡献者。**按任务跳到对应 crate 再读它的文档**，不必通读全仓。
> 冲突时**以代码为准**，并顺手更新本文。

## 0. 这是个什么项目

**AI 驱动的工作流自动化引擎**：把模糊需求转成**可存储、可调度、可复用**的执行计划（详见根 `README.md`，那里讲理念；本文件讲**代码在哪、怎么改**）。

两种执行模式（`README.md:22-27`）：

| | 周密模式 (Thorough) | 灵活模式 (Flexible) |
|---|---|---|
| 路径 | 澄清 → Coarse 规划 → ReAct 探路 → 固化脚本 | 自由执行 → 轨迹提取 → Coarse 提炼 → 保存计划 |
| 产出 | 固化计划（极少 AI 调用，可定时） | 灵活计划（保留少量 AI 调用，适应环境变化） |

代码实现：`planned-agent` crate 的 `flexible/`（灵活执行器）、`chat/`（对话与子 agent）、以及 `core::planner`（Coarse / ReAct / RePlanner）。

## 1. workspace 分层（10 个 crate）

依赖**只能自上而下**，这是硬约束。下表按层排列（`crates/*/Cargo.toml` 逐个核对）：

| 层 | crate (目录 → `name`) | 一句话职责 | 依赖的同仓 crate | 文档 |
|---|---|---|---|---|
| L0 | `crates/util` → `planned-agent-util` | 通用工具函数，与领域无关 | **无** | — |
| L0 | `crates/core` → `planned-agent-core` | **Agent 核心库：能力抽象与领域模型的家** | **无** | ⭐ `AGENTS.md`、`docs/core.md`（旧） |
| L0 | `crates/rag` → `planned-agent-rag` | 向量检索（Embedding / 存储 / 语义检索），自包含 | **无** | — |
| L1 | `crates/ai-openai` → `planned-agent-ai-openai` | 实现 `AiClient`，封装 async-openai | core | `docs/ai-openai.md` |
| L1 | `crates/mcp-rmcp` → `planned-agent-mcp-rmcp` | MCP 接入（持久化 + 运行时） | core | `README.md`、`docs/mcp-rmcp.md` |
| L1 | `crates/prompt-manager` → `planned-agent-prompt-manager` | 基于文件系统的 Prompt 模板加载/渲染 | core | `README.md` |
| L1 | `crates/tool-manager` → `planned-agent-tool-manager` | 统一工具管理器（MCP / 自定义 / 内置） | core | `ANALYSIS.md`、`docs/tool-manager.md` |
| L2 | `crates/ai-manager` → `planned-agent-ai-manager` | 多 AI 提供商客户端管理 | core, ai-openai | `docs/ai-manager.md` |
| L3 | `crates/planned-agent` → `planned-agent` (lib `planned_agent`) | **Plan-and-Execute 流水线** + `flexible` / `chat` 模块 | core, ai-openai, ai-manager, mcp-rmcp, prompt-manager, tool-manager, rag | `docs/planned-agent.md`、`docs/planned-agent/` |
| L4 | `crates/agent-gui` → `planned-agent-gui` | **Dioxus 0.7 桌面客户端**（唯一消费 `planned-agent` 的 crate） | core, ai-manager, mcp-rmcp, prompt-manager, tool-manager, rag, planned-agent | `docs/agent-gui-storage.md` |

**枢纽**：`planned-agent-core` 被 **7 个** crate 依赖 —— 改它的公开 API 前先全仓 grep 用法（细节见 `crates/core/AGENTS.md`）。

### 文档在哪（以及哪些不可信）

- **crate 级导航**：`crates/<crate>/AGENTS.md` —— 目前只有 `crates/core/AGENTS.md`，**这是最新最可靠的形态**。
- **crate 级说明**：`docs/<crate>.md`（core / ai-openai / ai-manager / mcp-rmcp / tool-manager / planned-agent …）；部分 crate 另有 `README.md`。
- **专题设计稿**：`docs/planned-agent/*.md`（灵活执行器、run service、输出契约、Coarse / ReAct 设计等）。
- ⚠️ **`docs/` 里的文档新旧不一**：`docs/design.md`、`docs/core.md` 是**早期**设计稿，其中描述的结构已有不存在者（例如 `docs/core.md` 里的 `crates/core/src/types.rs`、`factory/` 目录**现在都不存在**；`docs/design.md` 称「统一执行器属后续扩展」而已实现）。**照抄会走错，以代码为准。**

## 2. 按任务找入口

| 我要改… | 去这里 |
|---|---|
| AI 请求 / 流式 / token | `core::ai`（trait `AiClient`）→ 实现看 `crates/ai-openai` |
| 多提供商切换 | `crates/ai-manager` |
| 工具注册 / 校验 / 内置工具 | `crates/tool-manager`（抽象在 `core::tool_registry`） |
| MCP 服务器接入 | `crates/mcp-rmcp`（抽象在 `core::mcp`） |
| Prompt 模板加载 / 渲染 | `crates/prompt-manager`（抽象在 `core::prompt`） |
| 规划（Coarse / ReAct / RePlanner） | `core::planner`（含实现，不只抽象） |
| **灵活执行器** | `crates/planned-agent/src/flexible/`（设计见 `docs/planned-agent/flexible-executor.md`） |
| 对话 / 子 agent / 工具循环 | `crates/planned-agent/src/chat/` |
| 完整流水线编排 | `crates/planned-agent`（lib 顶层） |
| 宿主环境探测（os / 可用命令） | `core::host` |
| 桌面 UI | `crates/agent-gui`（入口 `src/main.rs:54`） |
| 向量检索 | `crates/rag` |
| 通用小工具函数 | `crates/util` |

## 3. 跨 crate 约定

1. **抽象在 core，实现在上层** —— 新能力先在 `core` 定义 trait + 数据类型，实现放下游。这条是避免 crate 相互依赖的关键，**详见 `crates/core/AGENTS.md`**。
2. **依赖方向单向**：下层**不认识**上层。要跨层调用，用 core 的 trait 做桥。
3. **异步 trait**：`#[async_trait]` + `: Send + Sync` + 返回 `anyhow::Result<_>`。
4. **错误**：默认 `anyhow::Result`；只有需要「可重试 / 恢复策略」时才用 `core::errors::PlanSystemError`。
5. **测试**：内联 `#[cfg(test)] mod tests` 为主；集成测试放 `crates/<crate>/tests/`；测试桩不要放 core，放在使用方（如 `planned-agent/src/flexible/testing.rs`）。
6. **文档沉淀习惯**：设计稿写 `docs/planned-agent/*.md`；crate 级导航写 `crates/<crate>/AGENTS.md`。

## 4. 构建与测试

```powershell
cargo build                          # 全仓
cargo test  -p planned-agent-core    # 单个 crate（推荐）
cargo test  -p <crate> --lib         # 单个 crate：优先带 --lib（见 §5 的 hang 警告）
cargo test  --workspace              # ⚠️ 目前会挂住，勿直接用（原因见 §5）

# 桌面 GUI
cd crates\agent-gui; cargo run
cargo test -p planned-agent-gui --bins   # GUI 侧测试
```

## 5. 已知的坑（别踩 / 别误判）

- ⚠️ **既有失败，不是你弄坏的**：`cargo test -p planned-agent` 有 3 个 `planner::coarse::llm_planner` 用例失败，原因是运行时找不到 prompt `planning/coarse_plan`（prompt 目录漂移）。除非任务就是修它，否则**不要**顺手改。
- ⚠️ **另一个既有 baseline 问题（会挂住，不是失败）**：`cargo test -p planned-agent-tool-manager`（**不带** `--lib`，以及因此 `cargo test --workspace`）会**卡住不返回** —— 停在集成测试 `tests/sub_agent_stream.rs`（纯 mock 用例、无网络，嫌疑在 `sub_agent_awaiting_user_action_then_resume` 的 resume 握手）。要跑该 crate 请用 `cargo test -p planned-agent-tool-manager --lib`（89 例）或指名目标（`--test cap_std_contract`，4 例）。**不要**为此顺手改 `sub_agent/`。
- ⚠️ **根 `README.md` 的 CLI 用法已过时**：它写的 `cargo run -- "..."` 不成立 —— `planned-agent` 现在是纯 lib，全仓**唯一**的 `fn main` 在 `crates/agent-gui/src/main.rs`。
- ⚠️ **根 `examples/` 不被 cargo 构建**：`examples/`（`mcp_tools.rs` / `prompt_manager.rs` / `stream_chat.rs`）不在任何 crate 目录下，根目录又不是 package，`cargo build` 不会碰它们 —— 但它们 `use` 真实 crate API，改动 API 时仍要注意。
- ℹ️ `planned-agent-util` 目前**没有任何 crate 依赖它**（孤儿 crate）。往里加东西前先确认真的有人要用。
- ℹ️ 全仓**没有 CI、没有 `build.rs`、没有 `[[bin]]`**（bin 由 package 名默认推出）。
