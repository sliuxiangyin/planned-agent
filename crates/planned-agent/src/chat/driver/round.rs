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
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::bridge::ToolExecutionBridge;
use super::confirm::await_confirm;
use super::prompt::inject_system_prompt;
use crate::chat::service::ChatEvent;
use crate::chat::state::{Command, History, State, Subscribers, ToolCallAccumulator};
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

/// 处理一个成功的流式 chunk：累积文本、推理、工具调用增量，并实时 emit 事件。
///
/// 每个 chunk 可能包含多个 `Choice`，每个 choice 的 delta 包含：
/// - `content`：文本增量 → 追加到 `text` 并 emit `TextDelta`；
/// - `reasoning_content`：推理增量 → 追加到 `reasoning` 并 emit `ReasoningDelta`；
/// - `tool_calls`：工具调用增量 → 按 `index` 累积到 `accumulators`，
///   首次获得 id+name 时 emit `ToolCallStart`，每次参数追加时 emit `ToolCallArgsDelta`。
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

        // ── 文本内容增量 ──
        if let Some(t) = &delta.content {
            if !t.is_empty() {
                *has_content = true;
                text.push_str(t);
                subscribers.emit(ChatEvent::Chat(CoreChatEvent::TextDelta(t.clone())));
            }
        }

        // ── 推理内容增量 ──
        if let Some(r) = &delta.reasoning_content {
            if !r.is_empty() {
                *has_reasoning = true;
                reasoning.push_str(r);
                subscribers.emit(ChatEvent::Chat(CoreChatEvent::ReasoningDelta(r.clone())));
            }
        }

        // ── 工具调用增量 ──
        if let Some(deltas) = &delta.tool_calls {
            for d in deltas {
                let index = d.index;
                let acc = accumulators
                    .entry(index)
                    .or_insert_with(ToolCallAccumulator::new);

                // 首次获得 tool_call id 时写入
                if let Some(id) = &d.id {
                    if acc.id.is_empty() {
                        acc.id.clone_from(id);
                    }
                }

                // 累积函数名和参数片段
                if let Some(func) = &d.function {
                    if let Some(name) = &func.name {
                        if acc.name.is_empty() {
                            acc.name.clone_from(name);
                        }
                    }
                    if let Some(args) = &func.arguments {
                        if !args.is_empty() {
                            acc.arguments.push_str(args);
                            // 参数增量实时推送给前端（需 id 已就绪）
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

                // id + name 首次齐备时 emit ToolCallStart（仅一次）
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

/// 打印当前轮次结束后 history 的摘要日志。
///
/// 逐条输出每条消息的 role、tool_calls 名称、内容预览（换行替换为空格），
/// 用于调试时快速定位 LLM 上下文是否正确。
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

/// 处理流式 chunk 错误：累计连续错误次数，达到阈值时终止消费。
///
/// 返回 `true` 表示已达到 `max_errors` 阈值，调用方应 break 退出流式消费循环。
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

/// 流式结束后，将每个累积完成的 tool_call 以 `ToolCallComplete` 事件推送给前端。
///
/// 跳过 id 为空的无效累积项；参数从原始 JSON 字符串解析为 `Value`，
/// 解析失败时降级为字符串字面量，避免丢失信息。
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

/// UI 工具处理结果的控制流信号。
enum UIActionOutcome {
    /// 工具已处理完毕，调用方可继续处理下一个。
    Continue,
    /// 用户取消了确认流程。
    UserCancelled,
    /// 子 agent 挂起等待用户确认，调用方应返回 `Suspended`。
    Suspended { run_id: String },
}

/// 处理单个 UI 工具调用（`request_user_action`）：解析参数、emit `UIActionRequest` 事件，
/// 然后按 `ui_strategy` 决定是阻塞等待用户确认还是挂起返回。
///
/// 正常返回表示该工具已处理完毕，调用方可继续处理下一个；
/// 返回 `UserCancelled` 或 `Suspended` 时调用方应立即停止循环。
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
            // 主 agent = None；子 agent = Some(run_id)，前端据此路由 resume
            session_id: run_id.clone(),
        }));

    match &ui_strategy {
        UIActionStrategy::BlockAndConfirm => {
            let confirmed = await_confirm(state, rx, queue, &call.id).await?;
            let Some((choice, action_id)) = confirmed else {
                // 用户取消 → 清理未闭合的 assistant tool_calls 消息
                state.history.clean_unclosed_assistant_tool_calls();
                state.subscribers.emit(ChatEvent::HistoryUpdated {
                    messages: state.history.snapshot(),
                });
                return Ok(UIActionOutcome::UserCancelled);
            };
            // 选择结果作为 text 闭合到 assistant（不产生 tool 消息）
            let choice_text = format!("\n\n[用户选择]: {choice} (action_id: {action_id})");
            state.history.close_pending_ui_tool_call(&choice_text);
        }
        UIActionStrategy::EmitAndSuspend => {
            // 子 agent：立即挂起返回。**保留** assistant(tool_calls)，不做
            // 清理——resume 时需据此找到 tool_call_id 压入 tool 消息闭合协议。
            // 挂起会话由外层 execute_streamed 存入 store（key = run_id），
            // 前端 resume 时经 signal_resume 唤醒继续。
            return Ok(UIActionOutcome::Suspended {
                run_id: run_id.unwrap_or_default(),
            });
        }
    }
    Ok(UIActionOutcome::Continue)
}

/// 后端工具执行结果的控制流信号。
enum BackendToolResult {
    /// 工具执行完毕（成功或错误），结果已入历史，可继续。
    Done,
    /// 执行期间被取消（子 agent 通道关闭），调用方应清理历史并返回 `Completed`。
    Cancelled,
}

/// 执行单个后端工具调用：根据是否需要流式分发到 `call_tool` / `call_tool_streamed`，
/// 将结果（成功或错误）作为 tool 消息推入历史，并 emit `ToolExecuted` 事件。
///
/// 返回 `BackendToolResult::Cancelled` 表示执行期间被取消，调用方应立即停止。
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
        info!("[round] === 第 {} 轮开始 ===", round);
        if state.cancelled.load(Ordering::SeqCst) {
            info!("[round] 已取消，break");
            break;
        }

        state
            .subscribers
            .emit(ChatEvent::Chat(CoreChatEvent::RoundStart { round }));

        // ── 构造请求 ──
        let tools = build_tool_definitions(state);
        // 打印下加载的工具
        // for tool in &tools {
        //     info!(
        //         "已加载工具: {} - {}",
        //         tool.function.name,
        //         tool.function.description.as_deref().unwrap_or("(无描述)")
        //     );
        // }

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

        let response_stream = state
            .ai_client
            .chat_completion_stream(req)
            .await
            .map_err(|e| anyhow!("chat_completion_stream 失败: {}", e))?;
        let mut inner = response_stream.stream;

        // 当前轮次 LLM 回复的累积缓冲区
        let mut text = String::new(); // 文本回复内容
        let mut reasoning = String::new(); // 推理/思考链内容
        let mut has_content = false; // 是否收到过非空文本增量
        let mut has_reasoning = false; // 是否收到过非空推理增量
                                       // tool_call 增量累积器：key = chunk 中的 index，value = 待拼接的完整 tool call
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

        // 流式结束后，若累积了未达阈值的错误且没有任何有效内容，将错误报告给前端
        if consecutive_stream_errors > 0 && !has_content && tool_calls_vec.is_empty() {
            state.subscribers.emit(ChatEvent::Error(last_stream_error));
        }

        // 通知前端：每个 tool_call 的参数已累积完毕，可展示完整调用信息
        emit_tool_call_completes(&state.subscribers, &accumulators);

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

        // ── 打印本轮结束后 history 摘要 ──
        log_history_summary(&state.history, round);

        // ── 结束条件 ──
        if tool_calls_vec.is_empty() {
            info!("[round] 无 tool_calls，break（本轮 LLM 未调用工具）");
            break;
        }

        if round >= state.config.lock().unwrap().max_tool_rounds {
            warn!(
                "chat: 达到 max_tool_rounds={}, 终止循环",
                state.config.lock().unwrap().max_tool_rounds
            );
            // 移除未闭合的 assistant tool_calls 消息（保持上下文干净，v1 同语义）
            state.history.pop_last_assistant_tool_calls_if_not_first();
            state.subscribers.emit(ChatEvent::HistoryUpdated {
                messages: state.history.snapshot(),
            });
            break;
        }

        // ── 执行工具：分离 UI 工具与普通后端工具 ──
        let (ui_calls, backend_calls): (Vec<_>, Vec<_>) = tool_calls_vec
            .iter()
            .partition(|tc| UI_TOOL_NAMES.contains(&tc.function.name.as_str()));

        info!(
            "[round] 后端工具 {} 个, UI 工具 {} 个",
            backend_calls.len(),
            ui_calls.len()
        );
        // 后端工具：直接执行，结果作为 tool 消息入历史
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

        // UI 工具（request_user_action）：不执行，发事件 + 按策略处理
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
