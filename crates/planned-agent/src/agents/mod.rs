//! Agent 模块 —— 灵活模式 Agent 实现。
//!
//! ## 架构
//!
//! - `FlexibleAgent` — 编排器：管理全局 context + 累积消息 + 4 步驱动
//! - `FlexibleExecuteAgent` — 纯执行器：子 agent，接收完整度文档，返回压缩 JSON
//! - `types` — 共享类型：`CompletenessDoc`、`ExecutionOutput`

pub mod flexible_agent;
pub mod test_sub_agent;

pub use flexible_agent::types::{CompletenessDoc, ExecutionOutput};
pub use flexible_agent::{FlexibleAgent, FlexibleExecuteAgent};
pub use test_sub_agent::{ChatSubAgent, test_sub_agent_tool};