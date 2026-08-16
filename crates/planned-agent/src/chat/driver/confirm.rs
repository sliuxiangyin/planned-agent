//! 用户确认等待逻辑。
//!
//! 等待 `Command::Confirm { tool_call_id, ... }`；若到达的是不匹配的 confirm
//! 发 `Error` 事件；若到达的是 send 则按串行队列语义入队；若无 token 关闭则报错；
//! 每 100ms 检查 `cancelled` 标志（用户在 awaiting 期间 stop）。

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::mpsc;

use crate::chat::state::{Command, RunState};
use crate::chat::service::ChatEvent;
use crate::chat::state::State;

/// 等待用户对指定 tool_call 的确认；返回 `Some((choice, action_id))` 表示确认，
/// `None` 表示用户取消。
pub(super) async fn await_confirm<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static>(
    state: &Arc<State<PM>>,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    queue: &mut VecDeque<Command>,
    tool_call_id: &str,
) -> Result<Option<(String, String)>> {
    *state.run_state.lock().unwrap() = RunState::AwaitingUserAction;
    let mut cancel_tick = tokio::time::interval(std::time::Duration::from_millis(100));
    cancel_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    Some(Command::Confirm {
                        tool_call_id: id,
                        choice,
                        action_id,
                    }) => {
                        if id == tool_call_id {
                            *state.run_state.lock().unwrap() = RunState::Running;
                            tracing::info!(
                                "chat: 用户确认 tool_call_id={}, action_id={}",
                                id, action_id
                            );
                            return Ok(Some((choice, action_id)));
                        }
                        state.subscribers.emit(ChatEvent::Error(format!(
                            "confirm_user_action: tool_call_id 不匹配（期望 {}，实际 {}），已忽略",
                            tool_call_id, id
                        )));
                    }
                    Some(Command::Send { message, done }) => {
                        // 串行队列：awaiting 期间的 send 排队，当前对话结束后处理
                        queue.push_back(Command::Send { message, done });
                    }
                    Some(Command::Reset) => {
                        // 串行队列：awaiting 期间的 reset 排队，当前对话结束后处理
                        queue.push_back(Command::Reset);
                    }
                    Some(Command::Resume { choice, action_id, done }) => {
                        // 串行队列：awaiting 期间的 resume 排队，当前对话结束后处理。
                        // 主 agent 的 await_confirm 期间理论上不会收到 Resume（Resume 由
                        // 子 agent 的 driver 消费），但为保持串行语义在此原样排队。
                        queue.push_back(Command::Resume { choice, action_id, done });
                    }
                    None => return Err(anyhow!("chat driver channel closed")),
                }
            }
            _ = cancel_tick.tick() => {
                if state.cancelled.load(Ordering::SeqCst) {
                    return Ok(None);
                }
            }
        }
    }
}