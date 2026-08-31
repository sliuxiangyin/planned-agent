//! chat 子 agent：基于 `ChatService` 实现 `SubAgentSessionRunner` / `SubAgentSession`。
//!
//! 核心思路：**不重复实现 ReAct 循环**，直接复用 `ChatService::send_text`，
//! 子 agent 与父 agent 共享同一个 `ToolRegistry`，但拥有独立的 prompt 和配置。
//!
//! 挂起-恢复采用业界 run_id 模型：
//! - `run_id` = 父 agent 调用本子 agent 时的 `invocation_id`（tool_call_id）；
//! - 挂起时子 agent 的 `run_conversation` 以 `EmitAndSuspend` 策略提前返回
//!   （`UIActionRequest` 已冒泡到前端），runner 返回 `AwaitingUserAction`；
//! - 恢复时前端经 `signal_resume` 唤醒 `execute_streamed`，取出 session 后
//!   `resume` 压入 tool 消息闭合协议，从 history **继续原对话**（不是新 send）。
//!
//! # 目录结构
//!
//! ```text
//! sub_agent/
//! ├── mod.rs        模块声明 + 对外重导出
//! ├── callback.rs   结果回调 trait + 决策枚举
//! ├── runner.rs     SubAgentRunner（工厂 + start 实现）
//! ├── session.rs    ChatSubAgentSession（挂起-恢复）
//! └── collect.rs    事件收集 + 回调决策 + 重试循环
//! ```

mod callback;
mod collect;
mod runner;
mod session;

pub use callback::{ResultDecision, SubAgentResultCallback};
pub use runner::SubAgentRunner;
pub use session::ChatSubAgentSession;
