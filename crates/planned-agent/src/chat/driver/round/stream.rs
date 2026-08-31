//! 流式响应处理：chunk 解析、错误处理、事件发射。

use std::collections::BTreeMap;

use planned_agent_core::ai::types::{MessageContent, MessageRole};
use planned_agent_tool_manager::ToolRegistry;
use tracing::{info, warn};

use crate::chat::service::ChatEvent;
use crate::chat::state::{History, Subscribers, ToolCallAccumulator};
use planned_agent_core::events::ChatEvent as CoreChatEvent;
use serde_json::Value;

/// 处理流式 chunk：提取 content、reasoning、tool_calls 并 emit 事件。
pub(super) fn process_stream_chunk(
    subscribers: &Subscribers,
    registry: &ToolRegistry,
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
                            if acc.start_emitted {
                                // Start 已发射，直接广播
                                subscribers.emit(ChatEvent::Chat(
                                    CoreChatEvent::ToolCallArgsDelta {
                                        id: acc.id.clone(),
                                        delta: args.clone(),
                                    },
                                ));
                            } else if !acc.id.is_empty() {
                                // Start 尚未发射，暂存缓冲区，等 Start 后 flush
                                acc.pending_deltas.push(args.clone());
                            }
                        }
                    }
                }
                // id + name 都收集到后，先发射 Start，再 flush 缓冲区
                if !acc.id.is_empty() && !acc.name.is_empty() && !acc.start_emitted {
                    let source = registry.get_metadata(&acc.name).map(|m| m.source.clone());
                    subscribers.emit(ChatEvent::Chat(CoreChatEvent::ToolCallStart {
                        id: acc.id.clone(),
                        name: acc.name.clone(),
                        source,
                    }));
                    acc.start_emitted = true;
                    for delta in acc.pending_deltas.drain(..) {
                        subscribers.emit(ChatEvent::Chat(CoreChatEvent::ToolCallArgsDelta {
                            id: acc.id.clone(),
                            delta,
                        }));
                    }
                }
            }
        }
    }
}

/// 记录历史摘要到日志。
pub(super) fn log_history_summary(history: &History, round: usize) {
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

/// 处理流式错误，返回 true 表示应终止消费。
pub(super) fn process_stream_error(
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

/// 发射 ToolCallComplete 事件。
pub(super) fn emit_tool_call_completes(
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
