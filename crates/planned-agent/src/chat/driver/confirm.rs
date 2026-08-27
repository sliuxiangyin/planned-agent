//! 用户确认等待逻辑。

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::mpsc;

use crate::chat::state::{Command, RunState};
use crate::chat::service::ChatEvent;
use crate::chat::state::State;

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
                        queue.push_back(Command::Send { message, done });
                    }
                    Some(Command::Reset) => {
                        queue.push_back(Command::Reset);
                    }
                    Some(Command::Resume { choice, action_id, done }) => {
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
