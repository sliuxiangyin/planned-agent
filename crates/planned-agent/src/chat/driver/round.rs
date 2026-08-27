//! 一次 [`ChatService::send`] 引发的完整多轮对话。

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use futures::StreamExt;
use planned_agent_core::ai::types::{
    ChatCompletionRequest, FunctionCall, Message, MessageContent, MessageRole, ToolCall, ToolType,
};
use planned_agent_core::events::ChatEvent as CoreChatEvent;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::bridge::ToolExecutionBridge;
use super::confirm::await_confirm;
use super::prompt::inject_system_prompt;
use crate::chat::service::ChatEvent;
use crate::chat::state::{Command, History, State, Subscribers, ToolCallAccumulator};
use crate::chat::tools::{build_tool_definitions, parse_ui_actions, UI_TOOL_NAMES};

pub(super) enum UIActionStrategy {
    BlockAndConfirm,
    EmitAndSuspend,
}

pub(super) enum ConversationOutcome {
    Completed,
    Suspended { run_id: String },
}

fn process_stream_chunk(
    subscribers: &Subscribers,
    chunk: planned_agent_core::ai::types::ChatCompletionChunk,
    text: &mut String,
    reasoning: &mut String,
    has_content: &mut bool,
    has_reasoning: &mut bool,
    accumulators: &mut BTreeMap<u32, ToolCallAccumulator>,
) {
    for choice in chunk.choices {
        let delta = &choice.delta;
        if let Some(t) = &delta.content {
            if !t.is_empty() {
                *has_content = true;
                text.push_str(t);
                subscribers.emit(ChatEvent::Chat(CoreChatEvent::TextDelta(t.clone())));
            }
        }
        if let Some(r) = &delta.reasoning_content {
            if !r.is_empty() {
                *has_reasoning = true;
                reasoning.push_str(r);
                subscribers.emit(ChatEvent::Chat(CoreChatEvent::ReasoningDelta(r.clone())));
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
                                subscribers.emit(ChatEvent::Chat(
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
                    subscribers.emit(ChatEvent::Chat(CoreChatEvent::ToolCallStart {
                        id: acc.id.clone(),
                        name: acc.name.clone(),
                    }));
                    acc.start_emitted = true;
                }
            }
        }
    }
}

fn log_history_summary(history: &History, round: usize) {
    let snap = history.snapshot();
    info!(
        "[round] === 第 {} 轮结束，history 共 {} 条消息 ===",
        round,
        snap.len()
    );
    for (i, msg) in snap.iter().enumerate() {
        let role = match msg.role {
            MessageRole::System => "System",
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::Tool => "Tool",
        };
        let content_preview = msg
            .content
            .as_ref()
            .map(|c| {
                let text = match c {
                    MessageContent::Text { text } => text.as_str(),
                    MessageContent::ToolResult { content, .. } => content.as_str(),
                    _ => "",
                };
                text.to_string()
            })
            .unwrap_or_else(|| "(无内容)".to_string());
        let tool_calls_info = msg
            .tool_calls
            .as_ref()
            .map(|tcs| {
                let names: Vec<&str> = tcs.iter().map(|tc| tc.function.name.as_str()).collect();
                format!(" tool_calls={:?}", names)
            })
            .unwrap_or_default();
        info!(
            "[round]   [{}] {}{}: {}",
            i,
            role,
            tool_calls_info,
            content_preview.replace('\n', " ")
        );
    }
    info!("[round] === history 摘要结束 ===");
}

fn process_stream_error(
    consecutive_errors: &mut u32,
    last_error: &mut String,
    max_errors: u32,
    error: anyhow::Error,
) -> bool {
    *consecutive_errors += 1;
    let msg = format!(
        "Stream chunk error ({}/{}): {}",
        consecutive_errors, max_errors, error
    );
    warn!("{}", msg);
    *last_error = msg;
    if *consecutive_errors >= max_errors {
        warn!("chat: 连续 {} 次流式 chunk 错误，终止消费", max_errors);
        return true;
    }
    false
}

fn emit_tool_call_completes(
    subscribers: &Subscribers,
    accumulators: &BTreeMap<u32, ToolCallAccumulator>,
) {
    for acc in accumulators.values() {
        if acc.id.is_empty() {
            continue;
        }
        let arguments: Value = serde_json::from_str(&acc.arguments)
            .unwrap_or_else(|_| Value::String(acc.arguments.clone()));
        subscribers.emit(ChatEvent::Chat(CoreChatEvent::ToolCallComplete {
            id: acc.id.clone(),
            name: acc.name.clone(),
            arguments,
        }));
    }
}

enum UIActionOutcome {
    Continue,
    UserCancelled,
    Suspended { run_id: String },
}

async fn handle_ui_tool_call<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
    call: &ToolCall,
    ui_strategy: &UIActionStrategy,
    rx: &mut mpsc::UnboundedReceiver<Command>,
    queue: &mut VecDeque<Command>,
) -> Result<UIActionOutcome> {
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
            session_id: run_id.clone(),
        }));

    match &ui_strategy {
        UIActionStrategy::BlockAndConfirm => {
            let confirmed = await_confirm(state, rx, queue, &call.id).await?;
            let Some((choice, action_id)) = confirmed else {
                state.subscribers.emit(ChatEvent::HistoryUpdated {
                    messages: state.history.snapshot(),
                });
                return Ok(UIActionOutcome::UserCancelled);
            };
            // 用户选择结果作为 tool 消息写入，与其它工具一致
            let tool_content = serde_json::json!({
                "choice": choice,
                "action_id": action_id
            });
            state.history.push_tool(&call.id, &tool_content);
        }
        UIActionStrategy::EmitAndSuspend => {
            return Ok(UIActionOutcome::Suspended {
                run_id: run_id.unwrap_or_default(),
            });
        }
    }
    Ok(UIActionOutcome::Continue)
}

enum BackendToolResult {
    Done,
    Cancelled,
}

async fn execute_backend_tool_call<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &Arc<State<PM>>,
    call: &ToolCall,
    bridge: &dyn ToolExecutionBridge,
) -> Result<BackendToolResult> {
    let args: Value = serde_json::from_str(&call.function.arguments)
        .unwrap_or_else(|_| Value::String(call.function.arguments.clone()));
    info!(
        "[round] 执行工具: {} (id={}) args:{:?}",
        call.function.name, call.id, args
    );

    let outcome = if bridge.needs_stream(&call.function.name) {
        let (stream, handle) = bridge.create_stream(&call.function.name, &call.id);
        let result = state
            .tool_registry
            .call_tool_streamed(&call.function.name, args, &call.id, stream)
            .await;
        let _ = handle.await;
        result
    } else {
        state
            .tool_registry
            .call_tool(&call.function.name, args)
            .await
    };

    let (is_error, content) = match &outcome {
        Ok(o) => (o.result.is_error, o.result.content.clone()),
        Err(e) => {
            if state.cancelled.load(Ordering::SeqCst) {
                state.history.push_cancelled_tool(&call.id, "任务被中断");
                state.subscribers.emit(ChatEvent::HistoryUpdated {
                    messages: state.history.snapshot(),
                });
                return Ok(BackendToolResult::Cancelled);
            }
            warn!("Tool '{}' failed: {}", call.function.name, e);
            (true, Value::String(format!("Error: {}", e)))
        }
    };

    info!(
        "[round] 工具 {} 执行完毕: is_error={}",
        call.function.name, is_error
    );
    state.history.push_tool(&call.id, &content);
    state
        .subscribers
        .emit(ChatEvent::Chat(CoreChatEvent::ToolExecuted {
            id: call.id.clone(),
            name: call.function.name.clone(),
            is_error,
            content,
        }));

    Ok(BackendToolResult::Done)
}

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
        info!("[round] === 第 {} 轮开始 ===", round);
        if state.cancelled.load(Ordering::SeqCst) {
            info!("[round] 已取消，break");
            break;
        }

        state
            .subscribers
            .emit(ChatEvent::Chat(CoreChatEvent::RoundStart { round }));

        let tools = build_tool_definitions(state);

        let (temperature, max_tokens) = {
            let cfg = state.config.lock().unwrap();
            (cfg.temperature, cfg.max_tokens)
        };
        let messages = state.history.snapshot();
        info!("[round] 请求历史: {} 条消息", messages.len());
        for (i, m) in messages.iter().enumerate() {
            info!(
                "[round]   [{}] role={:?} content={} tool_calls={}",
                i,
                m.role,
                m.content
                    .as_ref()
                    .map(|c| format!("{:?}", c).chars().take(50).collect::<String>())
                    .unwrap_or("None".into()),
                m.tool_calls.as_ref().map(|t| t.len()).unwrap_or(0)
            );
        }
        let req = ChatCompletionRequest {
            model: state.ai_client.model_name().to_string(),
            messages,
            tools: Some(tools),
            temperature,
            max_tokens,
            stream: true,
            extra: Default::default(),
        };

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
        let mut last_stream_error = String::new();
        const MAX_STREAM_ERRORS: u32 = 5;
        while let Some(chunk_result) = inner.next().await {
            if state.cancelled.load(Ordering::SeqCst) {
                break;
            }
            match chunk_result {
                Ok(c) => {
                    consecutive_stream_errors = 0;
                    process_stream_chunk(
                        &state.subscribers,
                        c,
                        &mut text,
                        &mut reasoning,
                        &mut has_content,
                        &mut has_reasoning,
                        &mut accumulators,
                    );
                }
                Err(e) => {
                    if process_stream_error(
                        &mut consecutive_stream_errors,
                        &mut last_stream_error,
                        MAX_STREAM_ERRORS,
                        e,
                    ) {
                        break;
                    }
                }
            }
        }

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

        if consecutive_stream_errors > 0 && !has_content && tool_calls_vec.is_empty() {
            state.subscribers.emit(ChatEvent::Error(last_stream_error));
        }

        emit_tool_call_completes(&state.subscribers, &accumulators);

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
        // 跳过空消息（无 content 且无 tool_calls）
        if !has_content && tool_calls_vec.is_empty() {
            info!("[round] LLM 返回空消息，跳过写入");
            break;
        }

        // 统一写入，保留 tool_calls（包括 request_user_action）
        state.history.push_assistant(assistant_msg.clone());
        state
            .subscribers
            .emit(ChatEvent::Chat(CoreChatEvent::RoundEnd {
                message: assistant_msg,
            }));

        log_history_summary(&state.history, round);

        if tool_calls_vec.is_empty() {
            info!("[round] 无 tool_calls，break（本轮 LLM 未调用工具）");
            break;
        }

        if round >= state.config.lock().unwrap().max_tool_rounds {
            warn!(
                "chat: 达到 max_tool_rounds={}, 终止循环",
                state.config.lock().unwrap().max_tool_rounds
            );
            state.history.pop_last_assistant_tool_calls_if_not_first();
            state.subscribers.emit(ChatEvent::HistoryUpdated {
                messages: state.history.snapshot(),
            });
            break;
        }

        let (ui_calls, backend_calls): (Vec<_>, Vec<_>) = tool_calls_vec
            .iter()
            .partition(|tc| UI_TOOL_NAMES.contains(&tc.function.name.as_str()));

        info!(
            "[round] 后端工具 {} 个, UI 工具 {} 个",
            backend_calls.len(),
            ui_calls.len()
        );
        for call in &backend_calls {
            if state.cancelled.load(Ordering::SeqCst) {
                break;
            }
            match execute_backend_tool_call(state, call, bridge).await? {
                BackendToolResult::Done => {}
                BackendToolResult::Cancelled => {
                    return Ok(ConversationOutcome::Completed);
                }
            }
        }
        info!("[round] 所有后端工具执行完毕，round += 1，继续循环");

        for call in &ui_calls {
            if state.cancelled.load(Ordering::SeqCst) {
                break;
            }
            match handle_ui_tool_call(state, call, &ui_strategy, rx, queue).await? {
                UIActionOutcome::Continue => {}
                UIActionOutcome::UserCancelled => {
                    return Ok(ConversationOutcome::Completed);
                }
                UIActionOutcome::Suspended { run_id } => {
                    return Ok(ConversationOutcome::Suspended { run_id });
                }
            }
        }

        round += 1;
    }

    Ok(ConversationOutcome::Completed)
}
