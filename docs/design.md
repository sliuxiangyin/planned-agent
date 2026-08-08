# Planned Agent 项目设计

## 概述
本项目旨在创建一个 Rust 异步智能代理（planned-agent），具备以下核心能力：
1. **多 AI SDK 支持**：通过工厂模式抽象 AI 接口，支持流式输出和直接输出，便于后续适配不同 AI 服务（如 OpenAI、Anthropic 等）。
2. **多 MCP 服务器支持**：支持同时连接多个 MCP 服务器，工具自动路由到正确的服务器。
3. **灵活配置**：支持多个 AI 提供商和 MCP 服务器配置，可通过配置文件或命令行参数选择默认提供商/服务器。
4. **计划生成与执行**：通过 `LlmCoarsePlanner` 生成结构化粗粒度计划，再由 `DefaultReActAgent` 按步骤动态选择工具并执行；详细步骤分析、统一执行器和协调器仍属于后续扩展。
5. **提示工程**：提供强大的提示模板管理能力，支持变量替换、输出约束和验证，确保 LLM 响应质量。
6. **异步运行时**：基于 tokio 实现全异步架构，确保高并发和低延迟。

## 技术栈
- **语言**：Rust (2021 edition)
- **异步运行时**：tokio (多线程运行时)
- **AI SDK**：async-openai (官方 OpenAI 客户端，支持流式响应)
- **MCP 协议**：rmcp (官方 Rust MCP SDK)
- **序列化**：serde, serde_json
- **错误处理**：anyhow, thiserror
- **日志**：tracing, tracing-subscriber
- **HTTP 客户端**：reqwest (通过 async-openai 间接使用)
- **工具**：cargo-workspace (项目管理)

## 目录结构
建议使用 Cargo 工作空间来组织代码，实现模块解耦和独立编译。结构如下：

```
planned-agent/
├── Cargo.toml                 # 工作空间根配置
├── crates/
│   ├── core/                  # 核心抽象层
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs         # 核心 trait 定义
│   │       ├── ai/            # AI SDK 抽象
│   │       │   ├── mod.rs
│   │       │   ├── traits.rs  # AI 客户端 trait (支持流式/直接输出)
│   │       │   └── types.rs   # 通用类型定义
│   │       ├── factory/       # 客户端工厂
│   │       │   ├── mod.rs
│   │       │   └── ai_factory.rs  # AI 客户端工厂
│   │       ├── mcp/           # MCP 集成抽象
│   │       │   ├── mod.rs
│   │       │   └── traits.rs  # MCP 客户端 trait
│   │       ├── planner/       # 计划引擎抽象
│   │       │   ├── mod.rs
│   │       │   ├── coarse/    # 粗粒度计划接口与类型
│   │       │   ├── react/     # ReAct 接口与类型
│   │       │   ├── replanner/ # 重规划类型
│   │       │   └── validation/ # 计划验证类型
│   │       └── prompt/        # 提示工程抽象
│   │           ├── mod.rs
│   │           └── traits.rs  # 提示管理器 trait
│   ├── ai-openai/             # async-openai 适配器
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── client.rs      # OpenAI 客户端实现
│   │       └── streaming.rs   # 流式响应处理
│   ├── ai-manager/            # AI 客户端管理器
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs         # AI 管理器实现
│   ├── mcp-rmcp/              # rmcp 适配器
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── client.rs      # MCP 客户端实现
│   │       ├── manager.rs     # MCP 管理器（多服务器支持）
│   │       └── tools.rs       # 工具管理（多服务器支持）
│   ├── prompt-manager/        # 提示管理器实现
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── manager.rs     # 提示管理器实现
│   │       ├── loader.rs      # 文件加载器
│   │       ├── template.rs    # 模板引擎
│   │       └── config.rs      # 配置管理
│   ├── tool-manager/          # 工具管理器
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs         # 模块入口
│   │       ├── types.rs       # 核心类型定义
│   │       ├── executor.rs    # ToolExecutor trait
│   │       ├── registry.rs    # ToolRegistry 核心
│   │       ├── custom_tool.rs # CustomTool trait
│   │       ├── mcp_adapter.rs # MCP 适配器
│   │       ├── validator.rs   # 参数验证器
│   │       └── builtin/       # 内置工具
│   └── planned-agent/         # 主程序
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs        # 入口点
│           ├── config.rs      # 配置管理（支持多配置）
│           ├── agent.rs       # 代理核心逻辑
│           ├── cli.rs         # 命令行接口
│           └── planner/
│               ├── coarse/    # LlmCoarsePlanner
│               └── react/     # DefaultReActAgent
├── prompts/                   # 提示模板目录
│   ├── flexible/              # 灵活模式对话提示
│   │   ├── flexible_system.toml          # 系统角色提示（灵活模式：执行轨迹总结）
│   │   ├── flexible_clarity_check.toml   # 清晰度判断 message
│   │   ├── flexible_param_identify.toml  # 参数识别 message
│   │   ├── flexible_output_suggest.toml  # 输出建议 message
│   │   └── flexible_trace_extract.toml   # 轨迹提取 message
│   ├── thorough/              # 周密模式对话提示
│   │   └── thorough_system.toml          # 系统角色提示（周密模式：需求确认后生成）
│   ├── analysis/              # 分析提示
│   │   └── extract_info.toml  # 信息提取提示
│   └── planning/              # 计划提示
│       ├── coarse_plan.toml   # 粗粒度计划生成
│       ├── react_think.toml   # ReAct 思考
│       ├── react_act.toml     # ReAct 行动
│       └── react_observe.toml # ReAct 观察
├── examples/                  # 示例代码
│   ├── stream_chat.rs         # 流式对话示例
│   ├── mcp_tools.rs           # MCP 工具调用示例
│   └── prompt_manager.rs      # 提示管理器示例
├── tests/                     # 集成测试
└── docs/                      # 文档
    ├── design.md              # 项目总体设计
    ├── core.md                # 核心抽象层详细设计
    ├── ai-openai.md           # AI SDK 适配器详细设计
    ├── mcp-rmcp.md            # MCP 集成详细设计
    ├── tool-manager.md        # 工具管理器详细设计
    ├── planned-agent.md       # 主程序详细设计
    └── planned-agent/         # 主程序计划组件文档
        ├── coarse-planner.md  # 粗粒度计划器
        └── react-agent.md     # ReAct 执行组件
```

## 模块设计

本项目的模块设计分为以下几个部分，详细设计请参考对应文档：

1. [核心抽象层 (`crates/core`)](core.md)
2. [AI SDK 适配器 (`crates/ai-openai`)](ai-openai.md)
3. [AI 管理器 (`crates/ai-manager`)](ai-manager.md)
4. [MCP 集成 (`crates/mcp-rmcp`)](mcp-rmcp.md)
5. [工具管理器 (`crates/tool-manager`)](tool-manager.md)
6. [提示管理器 (`crates/prompt-manager`)](prompt-engineering.md)
7. [主程序 (`crates/planned-agent`)](planned-agent.md)
8. [主程序计划组件：粗粒度计划器](planned-agent/coarse-planner.md)
9. [主程序计划组件：ReAct 执行器](planned-agent/react-agent.md)

## 依赖项 (Cargo.toml)

### 工作空间根 `Cargo.toml`
```toml
[workspace]
members = [
    "crates/core",
    "crates/ai-openai",
    "crates/ai-manager",
    "crates/mcp-rmcp",
    "crates/prompt-manager",
    "crates/tool-manager",
    "crates/planned-agent",
]
resolver = "2"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
thiserror = "1"
async-trait = "0.1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

### 核心库 `crates/core/Cargo.toml`
```toml
[package]
name = "planned-agent-core"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
async-trait.workspace = true
```

### AI 适配器 `crates/ai-openai/Cargo.toml`
```toml
[package]
name = "planned-agent-ai-openai"
version = "0.1.0"
edition = "2021"

[dependencies]
planned-agent-core = { path = "../core" }
async-openai = { version = "0.26", features = ["chat-completion"] }
tokio.workspace = true
futures = "0.3"
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
async-trait.workspace = true
tracing.workspace = true
```

### MCP 适配器 `crates/mcp-rmcp/Cargo.toml`
```toml
[package]
name = "planned-agent-mcp-rmcp"
version = "0.1.0"
edition = "2021"

[dependencies]
planned-agent-core = { path = "../core" }
rmcp = { version = "0.16", features = ["client"] }
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
async-trait.workspace = true
tracing.workspace = true
```

### 工具管理器 `crates/tool-manager/Cargo.toml`
```toml
[package]
name = "planned-agent-tool-manager"
version = "0.1.0"
edition = "2021"
description = "统一工具管理器，支持 MCP、自定义和内置工具"

[dependencies]
planned-agent-core = { path = "../core" }
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
async-trait.workspace = true
tracing.workspace = true
chrono = "0.4"
uuid = { version = "1", features = ["v4"] }
```

### 提示管理器 `crates/prompt-manager/Cargo.toml`
```toml
[package]
name = "planned-agent-prompt-manager"
version = "0.1.0"
edition = "2021"

[dependencies]
planned-agent-core = { path = "../core" }
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
async-trait.workspace = true
tracing.workspace = true
tera = "1"
toml = "0.8"
walkdir = "2"
```

### 主程序 `crates/planned-agent/Cargo.toml`
```toml
[package]
name = "planned-agent"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "planned-agent"
path = "src/main.rs"

[dependencies]
planned-agent-core = { path = "../core" }
planned-agent-ai-openai = { path = "../ai-openai" }
planned-agent-ai-manager = { path = "../ai-manager" }
planned-agent-mcp-rmcp = { path = "../mcp-rmcp" }
planned-agent-tool-manager = { path = "../tool-manager" }
planned-agent-prompt-manager = { path = "../prompt-manager" }
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
clap = { version = "4", features = ["derive"] }
config = "0.13"
```

## 实现阶段

当前计划引擎已经具备可运行的最小链路：`LlmCoarsePlanner` 生成 `CoarseGrainedPlan`，`DefaultReActAgent` 逐个执行计划步骤。`DetailedPlanner`、统一 `Executor`、`Supervisor` 和完整 `PlanOrchestrator` 尚未实现。


### 阶段一：基础架构 (第1-2天)
1. 初始化 Cargo 工作空间
2. 实现核心 trait 和类型定义
3. 搭建项目骨架和目录结构

### 阶段二：AI SDK 集成 (第3-4天)
1. 实现 async-openai 适配器
2. 支持流式输出和直接输出
3. 实现工厂模式和配置管理

### 阶段三：MCP 集成 (第5-6天)
1. 实现 rmcp 适配器
2. 支持工具发现和调用
3. 实现工具定义转换
4. 实现连接管理和健康检查
5. 添加自动重连和重试机制

### 阶段四：提示工程 (第7-8天)
1. 实现 PromptManager trait 和类型定义
2. 实现文件加载器，支持 TOML、TXT、Markdown 格式
3. 实现模板引擎，支持变量替换和输出约束
4. 实现 JSON Schema 验证和响应解析
5. 创建示例提示模板

### 阶段五：主程序整合 (第9-10天)
1. 实现命令行界面
2. 整合 AI、MCP 和提示管理模块
3. 实现完整的对话流程
4. 集成计划引擎和提示工程

### 阶段六：测试和文档 (第11-12天)
1. 编写单元测试和集成测试
2. 创建示例代码
3. 完善文档

## 后续扩展
1. **更多 AI 提供商**：添加 Anthropic、Google Gemini 等适配器
2. **计划引擎完善**：补充详细步骤分析、统一执行器、监督器和动态重规划协调逻辑
3. **持久化**：保存对话历史和计划状态
4. **Web API**：提供 RESTful 或 GraphQL 接口
5. **插件系统**：支持自定义工具和功能扩展
6. **环境变量支持**：支持通过环境变量覆盖配置
7. **配置热重载**：支持运行时重新加载配置文件
8. **提示模板市场**：支持社区共享和下载提示模板
9. **多语言提示**：支持国际化提示模板
10. **提示版本控制**：支持提示模板的版本管理和回滚

## 配置示例

### 多配置格式（推荐）
```toml
# config.toml

# 多个 AI 提供商
[[ai_providers]]
name = "openai"
provider = "openai"
api_key = "sk-openai-key"
model = "gpt-4"
is_default = true

[[ai_providers]]
name = "anthropic"
provider = "anthropic"
api_key = "sk-anthropic-key"
model = "claude-3-opus"
is_default = false

[[ai_providers]]
name = "deepseek"
provider = "openai"
api_key = "sk-deepseek-key"
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com"
is_default = false

# 多个 MCP 服务器
[[mcp_servers]]
name = "playwright"
server_command = "npx"
server_args = ["@playwright/mcp@latest"]
transport = "stdio"
is_default = true

[[mcp_servers]]
name = "filesystem"
server_command = "npx"
server_args = ["@modelcontextprotocol/server-filesystem", "/home/user"]
transport = "stdio"
is_default = false

[[mcp_servers]]
name = "github"
server_command = "npx"
server_args = ["@modelcontextprotocol/server-github"]
transport = "stdio"
is_default = false

# Prompt 管理器配置
[prompt_manager]
prompt_dir = "./prompts"

[prompt_manager.template_engine]
auto_reload = true

[prompt_manager.cache]
enabled = true
max_size = 1000
ttl_seconds = 3600

[logging]
level = "info"
format = "pretty"
```

## 注意事项
1. **错误处理**：统一使用 `anyhow::Result`，关键错误使用 `thiserror` 定义具体类型
2. **异步安全**：确保所有 trait 对象是 `Send + Sync`，使用 `Arc<Mutex>` 共享状态
3. **配置安全**：敏感信息（API 密钥）通过环境变量或加密配置文件管理
4. **资源清理**：实现 `Drop` trait 或显式断开方法，确保 MCP 连接正确关闭
5. **日志追踪**：使用 `tracing` 记录关键操作，便于调试和监控
6. **连接稳定性**：MCP连接可能因网络、服务器重启等原因断开，必须实现自动重连和健康检查机制
7. **错误恢复**：对于可重试的错误（如网络超时、连接断开），应实现指数退避重试策略
8. **状态监控**：记录连接状态、调用成功率、延迟等指标，便于问题诊断和性能优化
9. **提示模板验证**：确保提示模板格式正确，变量定义完整，输出模式有效
10. **模板热更新**：支持运行时重新加载提示模板，避免重启应用
11. **缓存管理**：合理设置缓存大小和过期时间，避免内存泄漏
12. **响应验证**：验证 LLM 响应是否符合预期格式，处理格式错误的情况
