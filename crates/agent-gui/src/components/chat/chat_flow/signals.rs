//! 聊天信号容器 —— `ChatSignals` 纯数据操作。
//!
//! 单一气泡源：`bubbles`（历史）+ `active`（当前 turn 气泡组）。
//! 流式事件只增量更新 `active`（O(当前 turn 轮数)），`Done` 时整组并入 `bubbles`。
//!
//! 不依赖任何外部业务 crate（ChatService / ChatStorage），
//! 只操作 Signal 中的状态数据。业务流程在 `controller.rs` 中。

use std::collections::HashMap;

use dioxus::prelude::*;
use planned_agent::chat::storage::{ErrorType, StoreMessage};
use planned_agent::chat::SubscriptionGuard;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use super::types::{AgentEvent, AgentViewData, Bubble, PendingUI, ToolCallPhase, ToolViewData};

/// 消息状态（纯内存，Signal 均为 `Copy` 可直接进闭包/异步块）。
#[derive(Clone, Copy, PartialEq)]
pub struct ChatSignals {
    /// 历史气泡（已完成的 turn，`finish_turn` 并入）。
    pub bubbles: Signal<Vec<Bubble>, SyncStorage>,
    /// 当前 turn 气泡组（`send_message` → `Done`，流式增量更新的活跃区）。
    pub active: Signal<Vec<Bubble>, SyncStorage>,
    /// 子 agent 流式数据存储（key = tool_call_id）。
    pub agent_views: Signal<HashMap<String, AgentViewData>, SyncStorage>,
    pub pending_ui: Signal<Option<PendingUI>, SyncStorage>,
    pub input_text: Signal<String, SyncStorage>,
    pub pending_tool_call_id: Signal<Option<String>, SyncStorage>,
    pub subscription: Signal<Option<SubscriptionGuard>, SyncStorage>,
}

// ── 全量重建（历史加载 / 快照校准）───────────────────────────────────────

/// 从 `StoreMessage` 序列重建气泡（纯函数，仅用于历史加载 / 快照校准）。
///
/// 消息序列规则：
/// - User → 独立 user 气泡
/// - Assistant → 独立 assistant 气泡（reasoning + text + tool_calls）
/// - Tool → 正常工具按 `tool_call_id` 回填对应 `ToolViewData.result`；
///   `request_user_action` 的 Tool 消息：主 agent → 追加到 assistant 气泡文本；
///   子 agent → 追加到 `agent_views` 对应条目的 events
/// - 其他（System 等）→ 忽略
pub fn build_bubbles(messages: &[StoreMessage], mut agent_views: Option<&mut HashMap<String, AgentViewData>>) -> Vec<Bubble> {
    let mut bubbles: Vec<Bubble> = Vec::new();
    let mut tool_index: HashMap<String, (usize, usize)> = HashMap::new();
    let mut ui_action_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 追踪：当前 assistant 消息是否为子 agent，以及子 agent 在父 agent 里的 tool_call_id
    let mut last_is_agent_tool = false;
    let mut last_agent_tool_id: Option<String> = None;

    for sm in messages {
        match sm.message.role {
            MessageRole::User => {
                last_is_agent_tool = false;
                last_agent_tool_id = None;
                bubbles.push(Bubble {
                    is_assistant: false,
                    text: display_text(&sm.message).to_string(),
                    reasoning: String::new(),
                    is_streaming: false,
                    tool_calls: Vec::new(),
                });
            }
            MessageRole::Assistant => {
                last_is_agent_tool = sm.is_agent_tool;
                last_agent_tool_id = if sm.is_agent_tool {
                    sm.message.tool_calls.as_ref().and_then(|tcs| {
                        tcs.iter().find(|tc| tc.function.name != "request_user_action")
                            .map(|tc| tc.id.clone())
                    })
                } else {
                    None
                };
                let tool_calls: Vec<ToolViewData> = sm
                    .message
                    .tool_calls
                    .as_ref()
                    .map(|tcs| {
                        tcs.iter()
                            .filter(|tc| {
                                if tc.function.name == "request_user_action" {
                                    ui_action_ids.insert(tc.id.clone());
                                    false // 不创建 ToolViewData
                                } else {
                                    true
                                }
                            })
                            .map(|tc| ToolViewData {
                                tool_call_id: tc.id.clone(),
                                name: tc.function.name.clone(),
                                arguments: tc.function.arguments.clone(),
                                phase: ToolCallPhase::Completed,
                                result: None,
                                is_error: false,
                                is_sub_agent: sm.is_agent_tool,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let bubble_idx = bubbles.len();
                for (entry_idx, tc) in tool_calls.iter().enumerate() {
                    tool_index.insert(tc.tool_call_id.clone(), (bubble_idx, entry_idx));
                }
                bubbles.push(Bubble {
                    is_assistant: true,
                    text: display_text(&sm.message).to_string(),
                    reasoning: sm.message.reasoning_content.clone().unwrap_or_default(),
                    is_streaming: false,
                    tool_calls,
                });
            }
            MessageRole::Tool => {
                if let Some(id) = sm.message.tool_call_id.as_deref() {
                    if ui_action_ids.contains(id) {
                        if let Some(choice) = extract_choice(&sm.message) {
                            if last_is_agent_tool {
                                // 子 agent 的 request_user_action → 追加到 agent_views
                                if let (Some(views), Some(ref agent_id)) = (agent_views.as_deref_mut(), &last_agent_tool_id) {
                                    if let Some(av) = views.get_mut(agent_id) {
                                        av.events.push(AgentEvent::TextDelta(
                                            format!("\n\n---\n\n**{}**\n\n", choice)
                                        ));
                                    }
                                }
                            } else {
                                // 主 agent → 追加到父 assistant 气泡文本
                                if let Some(last_asst) = bubbles.iter_mut().rfind(|b| b.is_assistant) {
                                    last_asst.text.push_str(&format!(
                                        "\n\n---\n\n**{}**\n\n", choice
                                    ));
                                }
                            }
                        }
                    } else if let Some(&(b, e)) = tool_index.get(id) {
                        // 正常工具 → 回填 result
                        if let Some(result) = parse_tool_result(&sm.message) {
                            bubbles[b].tool_calls[e].result = Some(result);
                        }
                        if sm.is_error_type != ErrorType::None {
                            bubbles[b].tool_calls[e].phase = ToolCallPhase::Error;
                            bubbles[b].tool_calls[e].is_error = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    bubbles
}

/// 从 `Message` 取出可显示文本。
fn display_text(msg: &Message) -> &str {
    match &msg.content {
        Some(MessageContent::Text { text }) => text.as_str(),
        _ => "",
    }
}

/// 从 `request_user_action` Tool 消息的 content 中提取 `choice` 字段。
///
/// content 格式为 JSON：`{"choice":"approved","action_id":"..."}`，
/// 与 `handle_user_action` 实时路径的 `push_tool` 写入格式一致。
fn extract_choice(msg: &Message) -> Option<String> {
    let content_text = match &msg.content {
        Some(MessageContent::ToolResult { content, .. }) => content.as_str(),
        Some(MessageContent::Text { text }) => text.as_str(),
        _ => return None,
    };
    // 尝试解析 JSON，提取 choice 字段
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(content_text) {
        if let Some(choice) = json.get("choice").and_then(|v| v.as_str()) {
            return Some(choice.to_string());
        }
    }
    // 解析失败（非 JSON）→ 原样返回（兼容纯文本场景）
    let trimmed = content_text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 解析 Tool 消息的 content（JSON 字符串）为结果值。
///
/// 同时处理两种 content 变体：
/// - `MessageContent::Text`：实时 streaming 路径创建（`tool_call_executed`）
/// - `MessageContent::ToolResult`：服务端持久化后加载（`push_tool`）
fn parse_tool_result(msg: &Message) -> Option<serde_json::Value> {
    let content_text = msg.content.as_ref().and_then(|c| match c {
        MessageContent::Text { text } => Some(text.as_str()),
        MessageContent::ToolResult { content, .. } => Some(content.as_str()),
        _ => None,
    })?;
    Some(
        serde_json::from_str(content_text)
            .unwrap_or_else(|_| serde_json::Value::String(content_text.to_string())),
    )
}
