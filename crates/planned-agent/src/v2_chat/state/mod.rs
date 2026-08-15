//! 内部状态层：驱动对话所需的所有共享 / 累积状态。
//!
//! - `state.rs`：[`State`] 容器（`Arc` 共享）+ [`History`] / [`Subscribers`]
//!   封装类型（分别吞掉消息历史与订阅列表的锁）；
//! - `command.rs`：[`Command`]（driver 命令队列）/ [`RunState`]（运行状态）；
//! - `accumulator.rs`：[`ToolCallAccumulator`]（流式 tool_call 增量累积）。
//!
//! 本层仅供 `v2_chat` 内部使用（`pub(super)` 重导出），不对外暴露。

mod accumulator;
mod command;
mod state;

pub(super) use accumulator::ToolCallAccumulator;
pub(super) use command::{Command, RunState};
pub(crate) use state::{resolve_ai_client, History, State, Subscribers, SubscribersInner};
