# `planned-agent-tool-manager` 库分析报告

## 1. 库概览

**包名**: `planned-agent-tool-manager`
**版本**: 0.1.0
**Rust 版本**: 2021
**描述**: 统一工具管理器，支持 MCP、自定义和内置工具

这是一个为 `planned-agent` 项目设计的工具管理库，提供统一的工具注册、查询、执行和生命周期管理能力。核心设计目标是支持三种来源的工具：
1. **MCP 工具**：通过 MCP 协议连接的外部工具服务器
2. **自定义工具**：用户自定义的工具执行器
3. **内置工具**：系统预定义的工具集合
4. **子 Agent 工具**：支持会话式执行的子 Agent 工具

## 2. 架构设计

### 2.1 整体架构

```
┌─────────────────────────────────────────────────────────┐
│                    ToolRegistry (核心)                   │
├─────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐     │
│  │ MCP Adapter │  │ Custom Tool │  │ Builtin Tool│     │
│  │   (适配器)   │  │   (自定义)   │  │   (内置)    │     │
│  └─────────────┘  └─────────────┘  └─────────────┘     │
│                          │                              │
│  ┌─────────────────────────────────────────────────┐   │
│  │              Sub-Agent (子 Agent)                │   │
│  │  ┌─────────────┐  ┌─────────────┐               │   │
│  │  │   Stream    │  │   Session   │               │   │
│  │  │   (流式)    │  │   (会话)    │               │   │
│  │  └─────────────┘  └─────────────┘               │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
```

### 2.2 目录结构

```
src/
├── core/              # 核心模块
│   ├── mod.rs         # 导出核心类型
│   ├── registry.rs    # 核心注册表
│   ├── types.rs       # 类型定义
│   ├── executor.rs    # 执行器 trait
│   └── validator.rs   # 参数验证
│
├── sub_agent/         # 子 Agent 模块
│   ├── mod.rs         # 导出子 Agent 类型
│   ├── executor.rs    # SubAgentToolExecutor
│   ├── session.rs     # SubAgentSessionStore
│   └── stream.rs      # 流式事件协议
│
├── adapter/           # 适配器模块
│   ├── mod.rs         # 导出适配器类型
│   ├── mcp.rs         # McpManagerAdapter
│   └── custom.rs      # CustomToolExecutor
│
├── builtin/           # 内置工具
│   ├── mod.rs
│   ├── ai_tools.rs
│   ├── data_tools.rs
│   ├── doc_tools.rs
│   ├── file_tools.rs
│   ├── system_tools.rs
│   ├── text_tools.rs
│   └── web_tools.rs
│
└── lib.rs             # 库入口（含向后兼容 re-export）
```

### 2.3 设计原则

1. **依赖反转**：核心 trait（`ToolExecutor`, `McpManagerTrait`）定义在 `planned-agent-core` 中，本库提供实现
2. **线程安全**：使用 `RwLock` 保护共享状态，支持并发访问
3. **统一接口**：所有工具通过 `ToolRegistry` 统一注册和调用
4. **模块化**：每个工具类型独立实现，便于扩展
5. **会话式执行**：子 Agent 支持挂起-恢复模式，适合需要用户交互的场景
6. **向后兼容**：通过 `lib.rs` 中的 re-export 保持旧路径可用

## 3. 核心模块详解

### 3.1 `core/types.rs` - 类型定义

定义了三个核心类型：
- **`ToolOutcome`**: 工具调用结果，包含 `ToolResult` 和工具分类信息
- **`ToolMetadata`**: 工具元数据，包含来源、分类、优先级、标签等
- **`ToolRegistryStats`**: 注册表统计信息

### 3.2 `core/executor.rs` - 执行器

仅重新导出 `planned_agent_core::tool_registry::ToolExecutor` trait，保持接口一致性。

### 3.3 `core/registry.rs` - 工具注册表（核心）

**主要功能**：
1. **工具注册**：支持注册 MCP、自定义、内置和子 Agent 工具
2. **工具查询**：按名称、分类、来源、优先级等查询工具
3. **工具执行**：自动路由到正确的执行器
4. **工具管理**：启用/禁用、更新分类、统计信息

**关键数据结构**：
```rust
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Tool>>,           // 工具定义
    metadata: RwLock<HashMap<String, ToolMetadata>>, // 工具元数据
    mcp_manager: RwLock<Option<Arc<dyn McpManagerTrait>>>, // MCP 管理器
    custom_executors: RwLock<HashMap<String, Arc<dyn ToolExecutor>>>, // 自定义执行器
    builtin_executors: RwLock<HashMap<String, Arc<dyn ToolExecutor>>>, // 内置执行器
    sub_agent_executors: RwLock<HashMap<String, Arc<SubAgentToolExecutor>>>, // 子 Agent 执行器
    sub_agent_sessions: Arc<SubAgentSessionStore>, // 子 Agent 会话存储
    category_index: RwLock<HashMap<ToolCategory, Vec<String>>>, // 分类索引
}
```

**核心方法**：
- `register_tool()`: 注册工具（通用）
- `register_custom_tool()`: 注册自定义工具
- `register_builtin_tool()`: 注册内置工具
- `register_sub_agent()`: 注册子 Agent 工具
- `call_tool()`: 调用工具（非流式）
- `call_tool_streamed()`: 调用工具（流式，支持子 Agent）
- `signal_resume()`: 恢复挂起的子 Agent

### 3.4 `adapter/mcp.rs` - MCP 适配器

包装 `McpManagerTrait` 实现，提供统一接口。主要功能：
- 代理 MCP 工具调用
- 获取 MCP 工具列表
- 查找工具所属服务器
- 获取服务器分类配置

### 3.5 `sub_agent/stream.rs` - 流式执行

定义子 Agent 执行过程中的流式事件协议：

**事件类型**：
- `Status`: 生命周期状态（started/running/finished/failed）
- `TextDelta`: 文本增量（打字机效果）
- `ToolCall`: 子 Agent 内部工具调用
- `FinalSummary`: 最终结论摘要

**关键组件**：
- `ToolStreamEvent`: 流式事件结构
- `ToolStreamSender`: 事件发射句柄，支持同步/异步发射

### 3.6 `sub_agent/executor.rs` - 子 Agent 执行

**核心抽象**：
- `SubAgentSession`: 会话 trait，支持挂起-恢复
- `SubAgentSessionRunner`: 会话式执行体 trait
- `OneShotSubAgentRunner`: 一次性执行适配器
- `SubAgentToolExecutor`: 子 Agent 执行器，实现 `ToolExecutor`

**执行流程**：
1. 首次调用：`start()` → 可能返回 `AwaitingUserAction`
2. 挂起时：会话入存储，返回结构化 `ToolResult`
3. 恢复时：`signal_resume()` → `resume()` → 可能再次挂起
4. 完成时：`Done` → 清理会话

### 3.7 `sub_agent/session.rs` - 会话存储

管理子 Agent 挂起会话的生命周期：

**特性**：
- TTL 过期清理（默认 10 分钟）
- 惰性清理（每次操作时扫描）
- 防重入（`take` 即移除）
- 线程安全

**核心方法**：
- `upsert()`: 存入/更新会话
- `signal_resume()`: 发送恢复信号
- `take()`: 取出会话（防重入）
- `get()`: 检查会话是否存在
- `clear()`: 清空所有会话

### 3.8 `builtin/` - 内置工具

提供 7 类内置工具：

1. **`file_tools`**: 文件操作（读写、创建、删除）
2. **`text_tools`**: 文本处理（格式化、转换）
3. **`system_tools`**: 系统信息（CPU、内存、进程）
4. **`data_tools`**: 数据处理（JSON、CSV、统计）
5. **`ai_tools`**: AI 相关工具
6. **`web_tools`**: 网页抓取和解析
7. **`doc_tools`**: 文档处理

每个工具模块包含：
- `XxxToolsProvider`: 工具提供者，定义工具列表
- `XxxToolsExecutor`: 工具执行器，实现具体逻辑

## 4. 依赖关系

### 4.1 内部依赖

- **`planned-agent-core`**: 核心 trait 和类型定义
  - `tool_registry`: `ToolExecutor`, `BuiltinToolProvider`, `McpManagerTrait`
  - `mcp::types`: `Tool`, `ToolResult`
  - `events::ChatEvent`: 结构化聊天事件

### 4.2 外部依赖

| 依赖 | 版本 | 用途 |
|------|------|------|
| `tokio` | workspace | 异步运行时 |
| `serde` | workspace | 序列化/反序列化 |
| `serde_json` | workspace | JSON 处理 |
| `anyhow` | workspace | 错误处理 |
| `async-trait` | workspace | 异步 trait 支持 |
| `tracing` | workspace | 日志追踪 |
| `chrono` | 0.4 | 时间处理 |
| `uuid` | 1 (v4) | UUID 生成 |
| `which` | 4 | 命令查找 |
| `sysinfo` | 0.30 | 系统信息 |
| `readability-rust` | 0.1 | 网页可读性提取 |
| `htmd` | 0.5 | HTML 转 Markdown |
| `scraper` | 0.18 | HTML 解析 |

## 5. 代码质量评估

### 5.1 编译状态

✅ **编译成功**：所有代码均能正常编译

### 5.2 警告分析

**本库警告**：无（代码质量良好）

**依赖库警告**（`planned-agent` 主库）：
- 6 个未使用导入警告
- 2 个未使用变量警告
- 1 个未使用函数警告

### 5.3 代码特点

**优点**：
1. **良好的模块化**：每个功能模块职责清晰
2. **完善的文档**：模块和函数都有详细注释
3. **线程安全**：正确使用 `RwLock` 保护共享状态
4. **错误处理**：使用 `anyhow` 提供清晰的错误信息
5. **测试覆盖**：子 Agent 相关功能有完整测试
6. **向后兼容**：通过 re-export 保持旧路径可用

**潜在问题**：
1. **锁粒度**：`call_tool` 方法中存在多次获取锁的操作，可能影响性能
2. **内存使用**：`Tool` 和 `ToolMetadata` 被多次克隆
3. **硬编码值**：优先级默认值（10, 50, 100）和 TTL（10 分钟）是硬编码的

## 6. 测试覆盖情况

### 6.1 单元测试

**`sub_agent/session.rs`**：
- `upsert_take_roundtrip`: 会话存取往返测试
- `signal_resume_wakes_once`: 恢复信号唤醒测试
- `ttl_expired_session_is_purged`: TTL 过期清理测试

**`builtin/web_tools.rs`**：
- 多个网页解析测试（HTML 提取、格式转换、截断等）

### 6.2 集成测试

**`tests/sub_agent_stream.rs`**：
- `sub_agent_stream_forwards_events_and_links_call_id`: 流式事件转发测试
- `sub_agent_call_tool_non_streamed_works`: 非流式调用测试
- `non_sub_agent_tool_streamed_produces_no_events`: 非子 Agent 工具流式调用测试
- `sub_agent_unregister_cleans_executor`: 卸载清理测试
- `sub_agent_awaiting_user_action_then_resume`: 挂起-恢复完整流程测试
- `resume_with_unknown_session_id_errors`: 未知会话恢复错误测试
- `one_shot_runner_wrapper_works`: 一次性执行器测试
- `emit_event_carries_structured_chat_event`: 结构化事件传递测试

**测试覆盖率**：子 Agent 相关功能测试覆盖完整，其他模块测试较少。

## 7. 改进建议

### 7.1 性能优化

1. **减少锁竞争**：
   - 考虑使用 `dashmap` 替代 `RwLock<HashMap>` 提高并发性能
   - 优化 `call_tool` 方法，减少锁获取次数

2. **减少内存拷贝**：
   - 考虑使用 `Arc<Tool>` 而非克隆 `Tool`
   - 使用 `Cow<str>` 避免字符串克隆

### 7.2 功能增强

1. **工具版本管理**：
   - 支持工具版本升级和兼容性检查
   - 实现工具依赖关系管理

2. **工具链组合**：
   - 支持将多个工具组合成工具链
   - 实现工具间的管道传递

3. **监控和统计**：
   - 增加工具调用次数、耗时统计
   - 实现工具健康检查和熔断机制

### 7.3 代码质量

1. **增加测试覆盖**：
   - 为 `core/registry.rs` 增加单元测试
   - 为 `builtin` 工具增加集成测试

2. **文档完善**：
   - 增加使用示例
   - 完善 API 文档

3. **配置外部化**：
   - 将优先级默认值、TTL 等配置化
   - 支持通过配置文件自定义工具行为

### 7.4 安全性

1. **输入验证**：
   - 增强参数验证逻辑
   - 防止注入攻击

2. **权限控制**：
   - 实现工具调用权限控制
   - 支持工具调用审计日志

## 8. 总结

`planned-agent-tool-manager` 是一个设计良好的工具管理库，具有以下特点：

**优势**：
- 架构清晰，模块化程度高
- 支持多种工具来源（MCP、自定义、内置、子 Agent）
- 子 Agent 支持会话式执行，适合复杂交互场景
- 代码质量高，文档完善
- 测试覆盖子 Agent 核心功能
- 向后兼容性好

**适用场景**：
- 需要统一管理多种工具的 AI Agent 系统
- 需要支持工具动态注册/卸载的插件化系统
- 需要子 Agent 会话式执行的复杂工作流

**建议**：
- 优先优化性能瓶颈（锁竞争）
- 增加测试覆盖范围
- 考虑工具链组合功能
- 完善监控和统计能力

该库为 `planned-agent` 项目提供了坚实的工具管理基础，具备良好的扩展性和可维护性。