# Core 模块设计规范

> 本文档定义了 `planned-agent-core` 的模块组织规则和公共 API 边界。

## 1. 设计原则

### 1.1 最小公共接口原则

**核心目标**：只暴露必须对外的类型，减少外部依赖的复杂度。

```
外部调用者 ──→ 最小公共接口 ──→ 内部实现
```

### 1.2 模块分类

| 类别 | 说明 | 导出策略 |
|------|------|----------|
| **公共接口** | 对外提供的抽象 trait/接口 | `pub mod` + `pub use` |
| **内部实现** | 不对外暴露的实现细节 | `pub(crate)` 或私有 |

## 2. 模块组织规则

### 2.1 公共接口模块（可对外）

这些模块包含需要对外暴露的抽象接口：

| 模块 | 说明 | 导出内容 |
|------|------|----------|
| `ai` | AI 交互抽象（未来支持多 SDK） | `AiClient` trait, `AiProvider` |
| `prompt` | Prompt 管理抽象 | `PromptManager` trait |
| `types` | 全局共享类型 | 通用枚举、结构体 |

### 2.2 内部实现模块（不对外暴露）

这些模块的内部类型不应污染 core 的公共命名空间：

| 模块 | 说明 | 访问级别 |
|------|------|----------|
| `tool_registry` | 工具注册与执行 | 模块内部使用，不在 lib.rs 导出内部类型 |
| `planner` | 规划器实现 | 同上 |
| `mcp` | MCP 协议实现 | 同上 |
| `errors` | 错误类型定义 | 同上 |
| `events` | 事件系统 | 同上 |

## 3. 导出规则

### 3.1 允许的导出

```rust
// ✅ 公共 trait/接口
pub trait AiClient: Send + Sync { ... }

// ✅ 全局共享类型
pub struct Config { ... }

// ✅ 公共枚举
pub enum AiProvider { OpenAI, Claude, Gemini }

// ✅ 错误类型（如果需要外部处理）
pub struct AgentError { ... }
```

### 3.2 禁止的导出

```rust
// ❌ 内部实现细节
pub struct CoarsePlanner { ... }  // 应该只在 planner 模块内
pub struct ToolDefinition { ... }  // 应该只在 tool_registry 模块内
pub enum ExecutionEvent { ... }    // 应该只在 events 模块内

// ✅ 正确做法：通过 pub(crate) 限制
pub(crate) struct CoarsePlanner { ... }
```

### 3.3 lib.rs 导出模式

```rust
// lib.rs

// 公共接口模块 - 完全导出
pub mod ai;       // 对外暴露
pub mod prompt;   // 对外暴露
pub mod types;    // 对外暴露

// 内部模块 - 只声明，类型私有化
pub mod tool_registry;  // 内部实现
pub mod planner;        // 内部实现  
pub mod mcp;           // 内部实现
pub mod errors;        // 内部实现
pub mod events;        // 内部实现

// 不需要在 lib.rs 导出的子模块
// - tool_registry/types.rs 中的内部类型
// - planner/coarse/*.rs 中的实现细节
// - 等等
```

## 4. 子模块访问级别

### 4.1 规则

```
lib.rs
├── ai/           pub mod（对外可见）
│   └── traits.rs pub trait（对外可见）
├── tool_registry/
│   ├── mod.rs    pub mod（模块可见）
│   ├── traits.rs pub(crate) trait（仅 core 内部可见）
│   └── types.rs  pub(crate) struct（仅 core 内部可见）
└── planner/
    ├── mod.rs    pub mod
    ├── coarse/
    │   ├── mod.rs
    │   └── coarse_planner.rs  pub(crate) struct
    └── react/
        ├── mod.rs
        └── react_agent.rs  pub(crate) struct
```

### 4.2 访问级别说明

| 关键字 | 可见范围 | 用途 |
|--------|----------|------|
| `pub` | 全局 | 公共 API，对外暴露 |
| `pub(crate)` | core  crate 内 | 内部实现，可在 core 内部共享 |
| (无) | 模块内 | 完全私有，仅当前模块使用 |

## 5. 模块依赖关系

```
                    ┌─────────┐
                    │  types  │  (共享类型)
                    └────┬────┘
                         │
    ┌────────────────────┼────────────────────┐
    │                    │                    │
    ▼                    ▼                    ▼
┌─────────┐        ┌─────────┐          ┌─────────┐
│   ai    │        │ prompt  │          │ events  │
│ (trait) │        │ (trait) │          │ (trait) │
└────┬────┘        └────┬────┘          └────┬────┘
     │                   │                   │
     └─────────┬─────────┘                   │
               │                             │
               ▼                             ▼
        ┌─────────────┐              ┌─────────────┐
        │   planner   │              │    mcp      │
        └──────┬──────┘              └──────┬──────┘
               │                             │
               ▼                             │
        ┌─────────────┐                      │
        │tool_registry│                      │
        └─────────────┘                      │
                                              │
               ┌─────────────────────────────┘
               │
               ▼
        ┌─────────────┐
        │   errors    │
        └─────────────┘
```

## 6. 命名规范

### 6.1 文件命名

- 模块目录：`snake_case`（如 `tool_registry`）
- 模块文件：`mod.rs` + 子模块文件
- 类型文件：与类型同名的 `snake_case` 文件

### 6.2 类型命名

| 类型 | 命名规则 | 示例 |
|------|----------|------|
| Trait | `PascalCase` + Trait 后缀 | `AiClient`, `ToolExecutor` |
| Struct | `PascalCase` | `Config`, `ToolDefinition` |
| Enum | `PascalCase` | `AiProvider`, `EventType` |
| Error | `PascalCase` + Error 后缀 | `AgentError` |

## 7. 添加新模块检查清单

新增模块时，请确认：

- [ ] 该模块是否有类型需要对外暴露？
  - **是** → 在 `lib.rs` 中使用 `pub use` 导出
  - **否** → 保持模块内部私有
- [ ] 子模块的类型是否应该限制访问级别？
- [ ] 是否需要更新本文档？

## 8. 示例

### 8.1 正确示例

```rust
// core/src/lib.rs
pub mod ai;           // 公共接口
pub mod prompt;       // 公共接口
pub mod types;        // 共享类型
pub mod tool_registry; // 内部模块

// core/src/tool_registry/mod.rs
pub mod traits;
pub mod types;

// core/src/tool_registry/types.rs
pub struct ToolDefinition { ... }           // 对外暴露
pub(crate) struct InternalState { ... }     // 仅 core 内部
```

### 8.2 错误示例

```rust
// ❌ 不要在 lib.rs 中导出内部类型
pub use tool_registry::{ToolDefinition, InternalState};

// ❌ 不要将所有类型设为 pub
pub struct InternalState { ... }

// ✅ 正确的做法
// 在使用处直接引用完整路径
use crate::tool_registry::types::ToolDefinition;
```

## 9. 变更历史

| 日期 | 变更内容 |
|------|----------|
| 2026-07-30 | 初始规范，定义模块导出规则 |
