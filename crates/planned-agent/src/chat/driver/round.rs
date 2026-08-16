//! 一次 [`ChatService::send`] 引发的完整多轮对话。
//!
//! 包含：
//! - system prompt 注入；
//! - 多轮 LLM 流式调用 + 工具调用累积；
//! - 后端工具直接执行（结果作为 tool 消息入历史）；
//! - UI 工具（`request_user_action`）走 confirm 流程。

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use futures::StreamExt;
use planned_agent_core::ai::types::{
    ChatCompletionRequest, FunctionCall, Message, MessageContent, MessageRole, ToolCall, ToolType,
};
use planned_agent_core::events::ChatEvent as CoreChatEvent;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::warn;

use super::bridge::ToolExecutionBridge;
use super::confirm::await_confirm;
use super::prompt::inject_system_prompt;
use crate::chat::service::ChatEvent;
use crate::chat::state::Command;
use crate::chat::state::State;
use crate::chat::state::ToolCallAccumulator;
use crate::chat::tools::{build_tool_definitions, parse_ui_actions, UI_TOOL_NAMES};

/// UI 工具（`request_user_action`）的行为策略。
///
/// 主 agent 与子 agent 的差别仅在 UI 交互时：
/// - 主 agent：阻塞在 `await_confirm`，等用户在**自己的**命令队列回传确认；
/// - 子 agent：emit `UIActionRequest`（带 run_id）后立即返回 `Suspended`，
///   由外层（runner → execute_streamed → 前端 resume）驱动恢复。
pub(super) enum UIActionStrategy {
    /// 主 agent：阻塞等 `Command::Confirm`。
    BlockAndConfirm,
    /// 子 agent：emit 事件后挂起返回。
    EmitAndSuspend,
}

/// 一次 [`run_conversation`] 的结果。
pub(super) enum ConversationOutcome {
    /// 正常完成（无挂起，或用户取消）。
    Completed,
    /// 子 agent 挂起等待用户确认（run_id 用于前端 resume 路由）。
    Suspended { run_id: String },
}

/// 执行一次 `send` 引发的完整对话（可含多轮 tool-call / 多次 UI 确认）。
pub(super) async fn run_conversation<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    queue: &mut VecDeque<Command>,
    ui_strategy: UIActionStrategy,
    bridge: &dyn ToolExecutionBridge,
) -> Result<ConversationOutcome> {
    inject_system_prompt(state).await?;

    let mut round = 1usize;

    loop {
        if state.cancelled.load(Ordering::SeqCst) {
            break;
        }

        state
            .subscribers
            .emit(ChatEvent::Chat(CoreChatEvent::RoundStart { round }));

        // ── 构造请求 ──
        let tools = build_tool_definitions(state);
        let (temperature, max_tokens) = {
            let cfg = state.config.lock().unwrap();
            (cfg.temperature, cfg.max_tokens)
        };
        let req = ChatCompletionRequest {
            model: state.ai_client.model_name().to_string(),
            messages: state.history.snapshot(),
            tools: Some(tools),
            temperature,
            max_tokens,
            stream: true,
            extra: Default::default(),
        };

        // ── 流式消费 ──
        let response_stream = state
            .ai_client
            .chat_completion_stream(req)
            .await
            .map_err(|e| anyhow!("chat_completion_stream 失败: {}", e))?;
        let mut inner = response_stream.stream;

        let mut text = String::new();
        let mut reasoning = String::new();
        let mut has_content = false;
        let mut has_reasoning = false;
        let mut accumulators: BTreeMap<u32, ToolCallAccumulator> = BTreeMap::new();

        let mut consecutive_stream_errors = 0u32;
        const MAX_STREAM_ERRORS: u32 = 5;
        while let Some(chunk_result) = inner.next().await {
            if state.cancelled.load(Ordering::SeqCst) {
                break;
            }
            match chunk_result {
                Ok(c) => {
                    consecutive_stream_errors = 0;
                    for choice in c.choices {
                        let delta = &choice.delta;

                        if let Some(t) = &delta.content {
                            if !t.is_empty() {
                                has_content = true;
                                text.push_str(t);
                                state
                                    .subscribers
                                    .emit(ChatEvent::Chat(CoreChatEvent::TextDelta(t.clone())));
                            }
                        }
                        if let Some(r) = &delta.reasoning_content {
                            if !r.is_empty() {
                                has_reasoning = true;
                                reasoning.push_str(r);
                                state
                                    .subscribers
                                    .emit(ChatEvent::Chat(CoreChatEvent::ReasoningDelta(r.clone())));
                            }
                        }
                        if let Some(deltas) = &delta.tool_calls {
                            for d in deltas {
                                let index = d.index;
                                let acc = accumulators
                                    .entry(index)
                                    .or_insert_with(ToolCallAccumulator::new);
                                if let Some(id) = &d.id {
                                    if acc.id.is_empty() {
                                        acc.id.clone_from(id);
                                    }
                                }
                                if let Some(func) = &d.function {
                                    if let Some(name) = &func.name {
                                        if acc.name.is_empty() {
                                            acc.name.clone_from(name);
                                        }
                                    }
                                    if let Some(args) = &func.arguments {
                                        if !args.is_empty() {
                                            acc.arguments.push_str(args);
                                            if !acc.id.is_empty() {
                                                state.subscribers.emit(ChatEvent::Chat(
                                                    CoreChatEvent::ToolCallArgsDelta {
                                                        id: acc.id.clone(),
                                                        delta: args.clone(),
                                                    },
                                                ));
                                            }
                                        }
                                    }
                                }
                                if !acc.id.is_empty() && !acc.name.is_empty() && !acc.start_emitted {
                                    state
                                        .subscribers
                                        .emit(ChatEvent::Chat(CoreChatEvent::ToolCallStart {
                                            id: acc.id.clone(),
                                            name: acc.name.clone(),
                                        }));
                                    acc.start_emitted = true;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    consecutive_stream_errors += 1;
                    warn!(
                        "Stream chunk error ({}/{}): {}",
                        consecutive_stream_errors, MAX_STREAM_ERRORS, e
                    );
                    if consecutive_stream_errors >= MAX_STREAM_ERRORS {
                        warn!("chat: 连续 {} 次流式 chunk 错误，终止消费", MAX_STREAM_ERRORS);
                        break;
                    }
                }
            }
        }

        // ── tool_calls 累积完成 ──
        let tool_calls_vec: Vec<ToolCall> = accumulators
            .values()
            .filter(|acc| !acc.id.is_empty())
            .map(|acc| ToolCall {
                id: acc.id.clone(),
                r#type: ToolType::Function,
                function: FunctionCall {
                    name: acc.name.clone(),
                    arguments: acc.arguments.clone(),
                },
            })
            .collect();

        for acc in accumulators.values() {
            if acc.id.is_empty() {
                continue;
            }
            let arguments: Value = serde_json::from_str(&acc.arguments)
                .unwrap_or_else(|_| Value::String(acc.arguments.clone()));
            state
                .subscribers
                .emit(ChatEvent::Chat(CoreChatEvent::ToolCallComplete {
                    id: acc.id.clone(),
                    name: acc.name.clone(),
                    arguments,
                }));
        }

        // ── assistant 消息入历史 ──
        let assistant_msg = Message {
            role: MessageRole::Assistant,
            content: if has_content {
                Some(MessageContent::Text { text })
            } else {
                None
            },
            tool_calls: if tool_calls_vec.is_empty() {
                None
            } else {
                Some(tool_calls_vec.clone())
            },
            tool_call_id: None,
            name: None,
            reasoning_content: if has_reasoning { Some(reasoning) } else { None },
            ..Default::default()
        };
        state.history.push_assistant(assistant_msg.clone());
        state
            .subscribers
            .emit(ChatEvent::Chat(CoreChatEvent::RoundEnd {
                message: assistant_msg,
            }));

        // ── 结束条件 ──
        if tool_calls_vec.is_empty() {
            break;
        }
        if round >= state.config.lock().unwrap().max_tool_rounds {
            warn!(
                "chat: 达到 max_tool_rounds={}, 终止循环",
                state.config.lock().unwrap().max_tool_rounds
            );
            // 移除未闭合的 assistant tool_calls 消息（保持上下文干净，v1 同语义）
            state.history.pop_last_assistant_tool_calls_if_not_first();
            break;
        }

        // ── 执行工具：分离 UI 工具与普通后端工具 ──
        let (ui_calls, backend_calls): (Vec<_>, Vec<_>) = tool_calls_vec
            .iter()
            .partition(|tc| UI_TOOL_NAMES.contains(&tc.function.name.as_str()));

        // 后端工具：直接执行，结果作为 tool 消息入历史
        for call in &backend_calls {
            if state.cancelled.load(Ordering::SeqCst) {
                break;
            }
            let args: Value = serde_json::from_str(&call.function.arguments)
                .unwrap_or_else(|_| Value::String(call.function.arguments.clone()));

            let outcome = if bridge.needs_stream(&call.function.name) {
                // 子 agent 工具：使用流式调用，创建 stream 接收事件并转发
                let (stream, handle) = bridge.create_stream(&call.function.name, &call.id);
                let result = state
                    .tool_registry
                    .call_tool_streamed(&call.function.name, args, &call.id, stream)
                    .await;
                let _ = handle.await;
                result
            } else {
                // 普通工具：非流式调用
                state
                    .tool_registry
                    .call_tool(&call.function.name, args)
                    .await
            };
            let (is_error, content) = match &outcome {
                Ok(o) => (o.result.is_error, o.result.content.clone()),
                Err(e) => {
                    // 取消导致的子 agent 通道关闭（clear_sub_agent_sessions drop
                    // resume_tx）：不作为工具错误上报，直接跳出并清理历史，
                    // 让 finish_send 发出 Done { cancelled: true }。
                    if state.cancelled.load(Ordering::SeqCst) {
                        state.history.clean_unclosed_assistant_tool_calls();
                        return Ok(ConversationOutcome::Completed);
                    }
                    warn!("Tool '{}' failed: {}", call.function.name, e);
                    (true, Value::String(format!("Error: {}", e)))
                }
            };
            state.history.push_tool(&call.id, &content);
            state
                .subscribers
                .emit(ChatEvent::Chat(CoreChatEvent::ToolExecuted {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    is_error,
                    content,
                }));
        }

        // UI 工具（request_user_action）：不执行，发事件 + 按策略处理
        for call in &ui_calls {
            if state.cancelled.load(Ordering::SeqCst) {
                break;
            }
            let args: Value =
                serde_json::from_str(&call.function.arguments).unwrap_or_else(|_| Value::Null);
            let message = args["message"].as_str().unwrap_or("").to_string();
            let actions = parse_ui_actions(&args["actions"]);
            let run_id = state.config.lock().unwrap().run_id.clone();

            state
                .subscribers
                .emit(ChatEvent::Chat(CoreChatEvent::UIActionRequest {
                    message,
                    actions,
                    // 主 agent = None；子 agent = Some(run_id)，前端据此路由 resume
                    session_id: run_id.clone(),
                }));

            match &ui_strategy {
                UIActionStrategy::BlockAndConfirm => {
                    let confirmed = await_confirm(state, rx, queue, &call.id).await?;
                    let Some((choice, action_id)) = confirmed else {
                        // 用户取消 → 清理未闭合的 assistant tool_calls 消息
                        state.history.clean_unclosed_assistant_tool_calls();
                        return Ok(ConversationOutcome::Completed);
                    };
                    // 压入 tool 消息：{"choice": "...", "action_id": "..."}
                    state.history.push_tool(
                        &call.id,
                        &json!({ "choice": choice, "action_id": action_id }),
                    );
                }
                UIActionStrategy::EmitAndSuspend => {
                    // 子 agent：立即挂起返回。**保留** assistant(tool_calls)，不做
                    // 清理——resume 时需据此找到 tool_call_id 压入 tool 消息闭合协议。
                    // 挂起会话由外层 execute_streamed 存入 store（key = run_id），
                    // 前端 resume 时经 signal_resume 唤醒继续。
                    return Ok(ConversationOutcome::Suspended {
                        run_id: run_id.unwrap_or_default(),
                    });
                }
            }
        }

        round += 1;
    }

    Ok(ConversationOutcome::Completed)
}
