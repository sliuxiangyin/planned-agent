//! 一次 [`crate::chat::ChatService::send`] 的完成凭证。
//!
//! `send` 本身立即返回（不堵塞）；调用方如需等待这次发送引发的整段对话
//! 结束（回到 idle），可 `await ticket.wait()`。不等待则行为等同
//! fire-and-forget，完全依赖 `on_chat` 事件通道。

use anyhow::{anyhow, Result};
use tokio::sync::oneshot;

/// 一次 `send` 引发的对话结果。
///
/// 与旧版 `Result<(), String>` 相比，多了「挂起」这个第三态：
/// 子 agent 的 `run_conversation` 在遇到 UI 交互时以 `EmitAndSuspend`
/// 策略提前返回，此时本次 `send` 引发的 `run_conversation` 确实结束了，
/// 但子 agent 整体尚未完成——调用方（runner）需要据此返回
/// `AwaitingUserAction`，而不是当作正常完成或失败。
#[derive(Debug)]
pub enum SendOutcome {
    /// 正常完成（无挂起）。
    Completed,
    /// 子 agent 挂起等待用户确认（run_id 用于前端 resume 路由）。
    Suspended { run_id: String },
    /// 可终止性异常（LLM 请求失败、channel 关闭等）。
    Failed(String),
}

/// `send` 的完成凭证（可 `await`）。
#[derive(Debug)]
pub struct SendTicket {
    pub(crate) rx: oneshot::Receiver<SendOutcome>,
}

impl SendTicket {
    /// 等待本次 `send` 引发的整段对话结束。
    ///
    /// 正常结束（含用户 `stop()` 取消、子 agent 挂起）返回 `Ok(())`；
    /// 对话内部发生可终止异常（LLM 请求失败等）返回 `Err`。
    ///
    /// 注意：子 agent 挂起（`Suspended`）也映射为 `Ok(())`，因为本次
    /// `send` 引发的 `run_conversation` 确实结束了；需要区分挂起态的调用方
    /// 请改用 [`Self::wait_outcome`]。
    pub async fn wait(self) -> Result<()> {
        match self.wait_outcome().await {
            SendOutcome::Completed | SendOutcome::Suspended { .. } => Ok(()),
            SendOutcome::Failed(e) => Err(anyhow!(e)),
        }
    }

    /// 等待本次 `send` 的结果，返回区分完成/挂起/失败的三态。
    pub async fn wait_outcome(self) -> SendOutcome {
        match self.rx.await {
            Ok(outcome) => outcome,
            Err(_) => SendOutcome::Failed("chat driver 已退出，未收到完成信号".to_string()),
        }
    }
}
