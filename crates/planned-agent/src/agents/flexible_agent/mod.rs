//! `flexible_agent` 子模块 —— 灵活模式 Agent 实现的承载目录。
//!
//! - `FlexibleAgent` — 编排器：管理全局 context + 累积消息 + 4 步驱动
//! - `FlexibleExecuteAgent` — 纯执行器：子 agent，接收完整度文档，返回压缩 JSON
//! - `types` — 共享类型：`CompletenessDoc`、`ExecutionOutput`

pub mod flexible_agent;
pub mod flexible_execute_agent;
pub mod types;

pub use flexible_agent::FlexibleAgent;
pub use flexible_execute_agent::FlexibleExecuteAgent;
pub use types::{CompletenessDoc, ExecutionOutput};