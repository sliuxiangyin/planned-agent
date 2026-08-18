//! 后台 driver：串行消费命令队列，驱动多轮对话。
//!
//! - `mod.rs`：[`driver_loop`] —— 常驻 task，按顺序消费 `Command`，
//!   每次 `Send` 触发一次完整对话循环；
//! - `round.rs`：[`run_conversation`] —— 一次 send 的完整多轮 loop
//!   （LLM 流式 / 工具执行 / UI 确认）；
//! - `confirm.rs`：[`await_confirm`] —— 等待用户对 UI 卡片的确认；
//! - `prompt.rs`：[`inject_system_prompt`] —— system prompt 注入（幂等）。

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

/// 常驻 driver task：串行消费 `Command`。
///
/// - `Weak<State>`：所有 `ChatService` 实例都已 drop 时退出，避免 `State`
///   被 driver 自身强引用导致 task 永久挂起（生命周期泄漏）；
/// - `Send` 命令触发一次完整对话；`Confirm` 在 driver 顶层收到（没有正在
///   等待的 UI 交互）时发 `Error` 事件并忽略。
pub(super) async fn driver_loop<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static>(
    state: std::sync::Weak<State<PM>>,
    mut rx: mpsc::UnboundedReceiver<Command>,
) {
    // 二级队列：awaiting 期间到达的 send 排到这里，保持串行顺序
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
                // 每次新对话重置取消标志（stop 只影响当前对话，v1 同语义）
                state.cancelled.store(false, Ordering::SeqCst);
                // 回滚点：记录 user 消息写入前的位置；对话失败时整个对话回滚
                let mark = state.history.push_user(message);
                *state.run_state.lock().unwrap() = RunState::Running;

                let ui_strategy = pick_ui_strategy(&state);
                let bridge = bridge::SubAgentBridge::new(state.clone());
                let result = run_conversation(&state, &mut rx, &mut queue, ui_strategy, &bridge).await;
                finish_send(&state, result, mark, done);
            }
            Command::Confirm {
                tool_call_id,
                choice,
                action_id,
            } => {
                // driver 顶层收到 confirm：没有正在等待的 UI 交互
                state.subscribers.emit(ChatEvent::Error(format!(
                    "confirm_user_action: 当前没有等待中的 UI 交互 \
                     （tool_call_id={}, choice={}, action_id={}），已忽略",
                    tool_call_id, choice, action_id
                )));
            }
            Command::Reset => {
                // 会话重置：清空历史（在队列中串行执行，此时无活跃对话）
                state.history.clear();
            }
            Command::Resume {
                choice,
                action_id,
                done,
            } => {
                tracing::info!("[driver] 收到 Command::Resume，从挂起点继续子 agent 对话");
                // 找到挂起的 request_user_action 的 tool_call_id
                let Some(tool_call_id) = state.history.find_pending_ui_tool_call_id() else {
                    let msg = "resume: 历史中无挂起的 request_user_action，无法继续".to_string();
                    state.subscribers.emit(ChatEvent::Error(msg.clone()));
                    let _ = done.send(SendOutcome::Failed(msg));
                    continue;
                };
                // 回滚点：压入 tool 消息前的位置（resume 失败时恢复挂起态）
                let mark = state.history.snapshot().len();
                // 压入 tool 消息闭合 assistant(tool_calls)，然后从 history 继续
                state.history.push_tool(
                    &tool_call_id,
                    &serde_json::json!({ "choice": choice, "action_id": action_id }),
                );
                *state.run_state.lock().unwrap() = RunState::Running;

                // resume 只在子 agent 发生，继续用挂起返回策略（可能再次挂起）
                let bridge = bridge::SubAgentBridge::new(state.clone());
                let result = run_conversation(
                    &state,
                    &mut rx,
                    &mut queue,
                    UIActionStrategy::EmitAndSuspend,
                    &bridge,
                )
                .await;
                finish_send(&state, result, mark, done);
            }
        }
    }
    tracing::info!("[driver] driver loop 退出（channel 关闭或 State 已 drop）");
}

/// 根据 config 选择 UI 策略：主 agent 阻塞确认，子 agent 挂起返回。
fn pick_ui_strategy<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static>(
    state: &State<PM>,
) -> UIActionStrategy {
    if state.config.lock().unwrap().run_id.is_some() {
        UIActionStrategy::EmitAndSuspend
    } else {
        UIActionStrategy::BlockAndConfirm
    }
}

/// 收尾一次 `run_conversation`：发完成事件 / 挂起 / 错误回滚，并回填 done。
fn finish_send<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static>(
    state: &std::sync::Arc<State<PM>>,
    result: anyhow::Result<ConversationOutcome>,
    mark: usize,
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
            state.history.rollback_to(mark);
            state.subscribers.emit(ChatEvent::Error(e.to_string()));
            let _ = done.send(SendOutcome::Failed(e.to_string()));
        }
    }
}
