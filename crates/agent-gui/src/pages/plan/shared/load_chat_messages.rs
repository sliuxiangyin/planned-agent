//! 加载灵活模式聊天历史消息。
//!
//! 从 `chat_messages` 表加载 → 反序列化 `Message` → 重建 `ChatMessage`（含 `tool_call_entries`）。

use std::sync::Arc;

use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use crate::components::chat::chat_flow::{ChatMessage, ToolCallEntry, ToolCallPhase};
use crate::storage::repository::ChatMessageRepo;

/// 从 DB 加载灵活模式聊天消息，返回 `Vec<ChatMessage>`（按 sequence_order 排序）。
///
/// `tool_call_entries` 从 `Message.tool_calls` 重建（phase 默认 Completed）。
pub async fn load_chat_messages(
    plan_id: &str,
    repo: Arc<ChatMessageRepo>,
) -> Vec<ChatMessage> {
    let rows = match repo.find_by_plan_id(plan_id).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("加载聊天消息失败: {}", e);
            return Vec::new();
        }
    };

    let mut result = Vec::with_capacity(rows.len());

    for row in rows {
        // 反序列化 Message
        let message: Message = match serde_json::from_str(&row.message_json) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("反序列化消息失败 (id={}): {}", row.id, e);
                continue;
            }
        };

        // 从 Message.tool_calls 重建 tool_call_entries
        let tool_call_entries = build_tool_call_entries(&message);

        result.push(ChatMessage {
            message,
            sequence_order: row.sequence_order as u64,
            is_streaming: false,
            tool_call_entries,
        });
    }

    result
}

/// 从 `Message.tool_calls` 构建 `ToolCallEntry` 列表。
///
/// 加载时无法知道执行结果（is_error），默认标记为 Completed。
/// 后续可扩展：如果 `chat_messages` 表存了 `tool_result` 行，则匹配填充 result + is_error。
fn build_tool_call_entries(message: &Message) -> Vec<ToolCallEntry> {
    let Some(tool_calls) = &message.tool_calls else {
        return Vec::new();
    };

    tool_calls
        .iter()
        .map(|tc| ToolCallEntry {
            name: tc.function.name.clone(),
            phase: ToolCallPhase::Completed,
            arguments: tc.function.arguments.clone(),
            result: None,
            is_error: false,
        })
        .collect()
}
