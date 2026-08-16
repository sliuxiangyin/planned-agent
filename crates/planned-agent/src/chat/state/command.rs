//! 内部命令与运行状态枚举。
//!
//! - [`Command`]：`send` 与 `confirm_user_action` 统一入队，driver 串行消费；
//! - [`RunState`]：driver 的对外可见运行状态（供调用方快速查询 UI 卡片是否在等待）。

use tokio::sync::oneshot;

use crate::chat::service::SendOutcome;

/// 命令队列：`send` / `confirm_user_action` / `reset` / `resume` 统一入队，driver 串行消费。
pub(crate) enum Command {
    Send {
        message: planned_agent_core::ai::types::Message,
        done: oneshot::Sender<SendOutcome>,
    },
    Confirm {
        tool_call_id: String,
        choice: String,
        action_id: String,
    },
    /// 会话重置：driver 空闲后清空内部消息历史（模板热切换配套）。
    ///
    /// 通过命令队列串行执行，避免与正在运行的对话竞争 history：
    /// 当前对话（含 UI 确认等待）结束后才真正清空。
    Reset,
    /// 子 agent resume：压入 tool 消息闭合挂起的 `request_user_action`，
    /// 然后从 history 继续 `run_conversation`（**不是**新 `send`）。
    Resume {
        choice: String,
        action_id: String,
        done: oneshot::Sender<SendOutcome>,
    },
}

/// driver 的对外可见运行状态（供调用方快速查询 UI 卡片是否在等待）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunState {
    /// 无对话在跑。
    Idle,
    /// 对话执行中（LLM 流式 / 工具执行）。
    Running,
    /// 对话挂起，等待 `confirm_user_action` 恢复。
    AwaitingUserAction,
}
