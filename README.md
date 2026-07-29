# Planned Agent

一个 Rust 异步智能代理，支持多种 AI SDK（工厂模式）、集成 MCP 协议，并将用户输入转化为计划步骤。

## 功能特性

1. **多 AI SDK 支持**：通过工厂模式抽象 AI 接口，支持流式输出和直接输出
2. **MCP 协议集成**：连接 MCP 服务器，调用外部工具，并将工具定义传递给 AI 模型
3. **连接管理**：自动健康检查、重连机制、断路器模式
4. **异步架构**：基于 tokio 实现全异步，确保高并发和低延迟

## 项目结构

```
planned-agent/
├── Cargo.toml                 # 工作空间根配置
├── crates/
│   ├── core/                  # 核心抽象层
│   ├── ai-openai/             # async-openai 适配器
│   ├── mcp-rmcp/              # rmcp 适配器
│   └── planned-agent/         # 主程序
├── examples/                  # 示例代码
├── docs/                      # 文档
│   └── design.md              # 设计文档
└── config.toml                # 配置文件
```

## 快速开始

### 1. 配置

编辑 `config.toml` 文件，设置 AI 密钥和 MCP 服务器：

```toml
[ai]
provider = "openai"
api_key = "sk-your-api-key"
model = "gpt-4"

[mcp]
server_command = "npx"
server_args = ["-y", "@modelcontextprotocol/server-everything"]
transport = "stdio"
```

### 2. 运行

```bash
# 交互模式
cargo run -- --interactive

# 单次查询
cargo run -- "What is the weather today?"

# 使用流式输出
cargo run -- --stream --interactive
```

### 3. 示例

```bash
# 流式对话示例
cargo run --example stream_chat

# MCP 工具示例
cargo run --example mcp_tools
```

## 开发

### 构建

```bash
cargo build
```

### 测试

```bash
cargo test
```

### 检查代码

```bash
cargo clippy
```

## 配置选项

### AI 配置

- `provider`: AI 提供商（目前支持 "openai"）
- `api_key`: API 密钥
- `model`: 模型名称
- `max_tokens`: 最大 token 数
- `temperature`: 温度参数

### MCP 配置

- `server_command`: MCP 服务器命令
- `server_args`: 服务器参数
- `transport`: 传输方式（stdio, tcp, websocket）
- `timeout_secs`: 超时时间
- `max_retries`: 最大重试次数

### 日志配置

- `level`: 日志级别（debug, info, warn, error）
- `format`: 日志格式（pretty, json）

## 扩展

### 添加新的 AI 提供商

1. 在 `crates/` 下创建新的 crate（如 `ai-anthropic`）
2. 实现 `AiClient` trait
3. 在主程序的工厂模式中添加新的提供商

### 添加新的 MCP 服务器

修改配置文件中的 MCP 服务器命令和参数即可。

## 设计文档

详细设计文档请参阅 [docs/design.md](docs/design.md)。

## 许可证

MIT License



1. 通过耳机或者录音笔外部设备 每分钟 实时获取录制的音频，分析音频语句的完整度，提取语义，给出建议，转换为音频播放出来
2. 对未成交的整个通话分析，给出后续跟踪决策
3. openauto.js 自动化维护，针对抖音快手小红书页面结构变化更新即时维护，
4. 增加自动维护评论回复





┌─────────────────────────────────────────────────────────────┐
│                    Coarse Planning                          │
│            (确定目标和粗粒度步骤)                            │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│                    ReAct Agent                               │
│  ┌───────────────────────────────────────────────────────┐  │
│  │  循环执行                                              │  │
│  │  ┌─────────┐    ┌─────────┐    ┌─────────┐           │  │
│  │  │ 思考    │ →  │ 行动    │ →  │ 观察    │ → 循环    │  │
│  │  │(Think)  │    │(Act)    │    │(Observe)│           │  │
│  │  └─────────┘    └─────────┘    └─────────┘           │  │
│  │       ↑              ↓              ↓                 │  │
│  │       │         选择工具        分析输出              │  │
│  │       │         生成参数        判断是否完成          │  │
│  │       │         执行工具        调整下一步            │  │
│  │       └──────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘


​Terminal 778-804​ 这里有个问题 就是 #2  执行失败
导致 #3  也做和#2同样的动作，是不对的，
如果#2执行失败就不要后续动作了直接失败，或者应该#2 阶段重复执行，而不是让#3做#2的事情
