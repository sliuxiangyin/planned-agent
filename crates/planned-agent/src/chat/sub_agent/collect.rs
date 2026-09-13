//! 事件收集 + 回调决策 + 重试循环。

use std::sync::{Arc, Mutex};

use anyhow::Result;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::events::{ChatEvent as CoreChatEvent, UIQuestion};
use planned_agent_core::mcp::types::ToolResult;
use planned_agent_prompt_manager::FilePromptManager;
use planned_agent_tool_manager::{
    SubAgentRunOutcome, ToolStreamSender,
};
use serde_json::Value;
use tracing::{error, info};

use crate::chat::service::{ChatEvent, ChatService, SendOutcome, SendTicket};

use super::callback::{ResultDecision, SubAgentCallContext, SubAgentResultCallback};
use super::session::ChatSubAgentSession;

/// 监听子 agent 的事件流，转发到 `ToolStreamSender`，
/// 直到对话完成（`Completed`）或挂起（`Suspended`）。
///
/// `Completed` 分支会调用回调，根据 [`ResultDecision`] 决定：
/// - `Accept`：接受原始结果
/// - `Transform`：替换 content
/// - `Retry`：重发纠正消息给子 agent（最多重试 2 次）
/// - `Abort`：不重试，直接以失败结果收场（内容为失败原因）
pub(super) async fn collect_until_outcome(
    service: &ChatService<FilePromptManager>,
    ticket: SendTicket,
    stream: &ToolStreamSender,
    depth: u32,
    max_depth: u32,
    result_callback: Option<Arc<dyn SubAgentResultCallback>>,
    // 父 agent 传给该子 agent 的原始参数；仅用于填充回调的 `SubAgentCallContext`，
    // 或随挂起会话保留到 resume（见 `ChatSubAgentSession`）。
    arguments: Value,
) -> Result<SubAgentRunOutcome> {
    info!("[子agent] collect_until_outcome 开始，注册事件监听");

    // 克隆 stream 以便传入闭包（闭包需要 'static）
    let stream_clone = stream.clone();

    // 捕获挂起时 UIActionRequest 的 message / questions（用于构造 AwaitingUserAction）
    let ui_request: Arc<Mutex<Option<(String, Vec<UIQuestion>)>>> = Arc::new(Mutex::new(None));
    let ui_request_clone = ui_request.clone();

    // 注册临时事件监听：转发子 agent 内部事件，并捕获挂起 UI 信息。
    //
    // 转发规则：
    // - `UIActionRequest` → 直接转发为 `Chat(CoreChatEvent)`，主 agent GUI 需要
    //   直接处理交互卡片（弹出 PendingUI）。
    // - `RoundStart` / `RoundEnd` → 不转发，避免主 agent 创建多余气泡。
    // - 其余（TextDelta / ReasoningDelta / ToolCall* / ToolExecuted）→
    //   包装为 `SubChatEvent`，通过 `SubChat` 通道转发，GUI 按 `tool_call_id`
    //   路由到对应 `AgentView`。
    let tool_call_id_for_closure = stream.invocation_id().to_string();
    let _guard = service.on_chat_with_guard(move |event| {
        if let ChatEvent::Chat(chat_event) = &event {
            // 捕获 UIActionRequest 的 message / questions
            if let CoreChatEvent::UIActionRequest {
                message, questions, ..
            } = chat_event
            {
                *ui_request_clone.lock().unwrap() = Some((message.clone(), questions.clone()));
            }

            match chat_event {
                // UIActionRequest → 直接转发（主 agent GUI 处理交互卡片）
                CoreChatEvent::UIActionRequest { .. } => {
                    stream_clone.emit_event_sync(chat_event.clone());
                }
                // 其余 → 包装为 SubChat
                _ => {
                    stream_clone.emit_event_sync(CoreChatEvent::SubChat {
                        tool_call_id: tool_call_id_for_closure.clone(),
                        event: Box::new(chat_event.clone()),
                    });
                }
            }
        }
    });

    // 等待子 agent 对话结果（区分完成 / 挂起 / 失败）
    match ticket.wait_outcome().await {
        SendOutcome::Completed => {
            let mut last_text = extract_last_assistant_text(&service.history());
            info!("[子agent] 子 agent 完成，提取结果：{}", last_text);

            // ── 回调决策 + 重试循环 ──
            let max_retries = 2;
            let final_text = if let Some(cb) = &result_callback {
                // 本次调用上下文：核心库只透传参数，具体取哪个字段由回调决定。
                // 先 clone 一份，因为 `arguments` 在后面 Suspended 分支要 move 给挂起会话。
                let call_ctx = SubAgentCallContext {
                    agent_name: stream.tool_name().to_string(),
                    tool_call_id: stream.invocation_id().to_string(),
                    arguments: arguments.clone(),
                };
                let mut attempts = 0u32;
                loop {
                    let probe = ToolResult {
                        call_id: String::new(),
                        is_error: false,
                        content: Value::String(last_text.clone()),
                    };
                    match cb.on_result(&call_ctx, &probe).await {
                        ResultDecision::Accept => break last_text,
                        ResultDecision::Transform(new) => break new,
                        ResultDecision::Retry(msg) if attempts < max_retries => {
                            attempts += 1;
                            info!("[子agent] 回调要求重试 ({}/{}), 发送纠正消息", attempts, max_retries);
                            match service.send_text(msg) {
                                Ok(retry_ticket) => {
                                    match retry_ticket.wait_outcome().await {
                                        SendOutcome::Completed => {
                                            last_text = extract_last_assistant_text(&service.history());
                                            info!("[子agent] 重试完成，新结果：{}", last_text);
                                            continue;
                                        }
                                        other => {
                                            info!("[子agent] 重试未正常完成: {:?}，使用原始结果", other);
                                            break last_text;
                                        }
                                    }
                                }
                                Err(e) => {
                                    info!("[子agent] 重试发送失败: {}，使用原始结果", e);
                                    break last_text;
                                }
                            }
                        }
                        ResultDecision::Retry(_) => {
                            info!("[子agent] 重试次数耗尽，使用原始结果");
                            break last_text;
                        }
                        ResultDecision::Abort(reason) => {
                            // 外部动作失败（如流程状态写库）：重试同样会失败，立即以失败结果收场，
                            // 让父 agent 看到 is_error 的 tool result 并知道原因。
                            error!("[子agent] 回调要求中断本次调用：{}", reason);
                            return Ok(SubAgentRunOutcome::Done(ToolResult {
                                call_id: String::new(),
                                is_error: true,
                                content: Value::String(reason),
                            }));
                        }
                    }
                }
            } else {
                last_text
            };

            let result = ToolResult {
                call_id: String::new(),
                is_error: false,
                content: Value::String(final_text),
            };
            Ok(SubAgentRunOutcome::Done(result))
        }
        SendOutcome::Suspended { .. } => {
            info!("[子agent] 子 agent 挂起，构造 AwaitingUserAction");
            let (message, questions) = ui_request.lock().unwrap().take().unwrap_or_default();
            Ok(SubAgentRunOutcome::AwaitingUserAction {
                session: Box::new(ChatSubAgentSession::new(
                    service.clone(),
                    depth,
                    max_depth,
                    result_callback.clone(),
                    arguments,
                )),
                message,
                actions: serde_json::to_value(questions).unwrap_or_else(|_| Value::Array(vec![])),
            })
        }
        SendOutcome::Failed(e) => {
            info!("[子agent] 子 agent 失败: {}", e);
            Ok(SubAgentRunOutcome::Done(ToolResult {
                call_id: String::new(),
                is_error: true,
                content: Value::String(format!("子 agent 执行失败: {}", e)),
            }))
        }
    }
}

/// 从历史中提取最后一条 assistant 文本消息
fn extract_last_assistant_text(history: &[Message]) -> String {
    for msg in history.iter().rev() {
        if matches!(msg.role, MessageRole::Assistant) {
            if let Some(MessageContent::Text { text }) = &msg.content {
                if !text.is_empty() {
                    return text.clone();
                }
            }
        }
    }
    String::new()
}
