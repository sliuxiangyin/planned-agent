//! 后台 driver：串行消费命令队列，驱动多轮对话。

mod bridge;
mod confirm;
mod prompt;
mod round;

use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use tokio::sync::mpsc;

use crate::chat::service::{SendOutcome, ChatEvent};
use crate::chat::state::{Command, RunState, State};
use round::{run_conversation, ConversationOutcome, UIActionStrategy};

pub(super) async fn driver_loop<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static>(
    state: std::sync::Weak<State<PM>>,
    mut rx: mpsc::UnboundedReceiver<Command>,
) {
    let mut queue: VecDeque<Command> = VecDeque::new();

    loop {
        let cmd = match queue.pop_front() {
            Some(c) => Some(c),
            None => rx.recv().await,
        };
        let Some(cmd) = cmd else { break };
        let Some(state) = state.upgrade() else { break };

        match cmd {
            Command::Send { message, done } => {
                tracing::info!("[driver] 收到 Command::Send，开始 run_conversation");
                state.cancelled.store(false, Ordering::SeqCst);
                let store_id = state.history.push_user(message);
                *state.run_state.lock().unwrap() = RunState::Running;

                let ui_strategy = pick_ui_strategy(&state);
                let bridge = bridge::SubAgentBridge::new(state.clone());
                let result = run_conversation(&state, &mut rx, &mut queue, ui_strategy, &bridge).await;
                finish_send(&state, result, store_id, done);
            }
            Command::Confirm {
                tool_call_id,
                choice,
                action_id,
            } => {
                state.subscribers.emit(ChatEvent::Error(format!(
                    "confirm_user_action: 当前没有等待中的 UI 交互 \
                     （tool_call_id={}, choice={}, action_id={}），已忽略",
                    tool_call_id, choice, action_id
                )));
            }
            Command::Reset => {
                state.history.clear();
                state.subscribers.emit(ChatEvent::HistoryUpdated {
                    messages: state.history.snapshot(),
                });
            }
            Command::Resume {
                choice,
                action_id,
                done,
            } => {
                tracing::info!("[driver] 收到 Command::Resume，从挂起点继续子 agent 对话");
                let Some(store_id) = state.history.find_pending_ui_tool_call_id() else {
                    let msg = "resume: 历史中无挂起的 request_user_action，无法继续".to_string();
                    state.subscribers.emit(ChatEvent::Error(msg.clone()));
                    let _ = done.send(SendOutcome::Failed(msg));
                    continue;
                };
                // 找到挂起的 request_user_action 的 tool_call_id
                let tool_call_id = state.history.find_pending_tool_call_id().unwrap_or_default();
                // 用户选择结果作为 tool 消息写入
                let tool_content = serde_json::json!({
                    "choice": choice,
                    "action_id": action_id
                });
                state.history.push_tool(&tool_call_id, &tool_content);
                *state.run_state.lock().unwrap() = RunState::Running;

                let bridge = bridge::SubAgentBridge::new(state.clone());
                let result = run_conversation(
                    &state,
                    &mut rx,
                    &mut queue,
                    UIActionStrategy::EmitAndSuspend,
                    &bridge,
                )
                .await;
                finish_send(&state, result, store_id, done);
            }
        }
    }
    tracing::info!("[driver] driver loop 退出（channel 关闭或 State 已 drop）");
}

fn pick_ui_strategy<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static>(
    state: &State<PM>,
) -> UIActionStrategy {
    if state.config.lock().unwrap().run_id.is_some() {
        UIActionStrategy::EmitAndSuspend
    } else {
        UIActionStrategy::BlockAndConfirm
    }
}

fn finish_send<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static>(
    state: &std::sync::Arc<State<PM>>,
    result: anyhow::Result<ConversationOutcome>,
    _store_id: String,
    done: tokio::sync::oneshot::Sender<SendOutcome>,
) {
    *state.run_state.lock().unwrap() = RunState::Idle;

    match result {
        Ok(ConversationOutcome::Completed) => {
            tracing::info!("[driver] run_conversation 完成，发送 Done 事件");
            state.subscribers.emit(ChatEvent::Done {
                cancelled: state.cancelled.load(Ordering::SeqCst),
            });
            let _ = done.send(SendOutcome::Completed);
        }
        Ok(ConversationOutcome::Suspended { run_id }) => {
            tracing::info!("[driver] run_conversation 挂起 (run_id={})，不发 Done", run_id);
            let _ = done.send(SendOutcome::Suspended { run_id });
        }
        Err(e) => {
            tracing::info!("[driver] run_conversation 错误: {}", e);
            // 先闭合可能存在的未闭合 tool_calls
            round::close::close_unclosed_tool_calls(state);
            // 补一条 error assistant 消息
            round::close::close_orphaned_user(state, &format!("Error: {}", e));
            state.subscribers.emit(ChatEvent::Error(e.to_string()));
            let _ = done.send(SendOutcome::Failed(e.to_string()));
        }
    }
}
