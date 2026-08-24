//! `ChatStorage` trait 的 SQLite 实现 —— 基于 `ChatMessageRepo`。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use crate::components::chat::chat_flow::{ChatMessage, ChatStorage, ToolCallEntry, ToolCallPhase};
use crate::storage::repository::ChatMessageRepo;

/// 基于 SQLite（SeaORM）的 `ChatStorage` 实现。
pub struct ChatMessageStorage {
    repo: Arc<ChatMessageRepo>,
}

impl ChatMessageStorage {
    pub fn new(repo: Arc<ChatMessageRepo>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl ChatStorage for ChatMessageStorage {
    async fn persist_message(&self, plan_id: &str, msg: &ChatMessage) {
        // 幂等：同 plan_id + sequence_order 已存在则跳过
        // （RoundEnd 会全量重放本轮所有消息，避免重复写入）
        if let Ok(Some(_)) = self
            .repo
            .find_by_plan_and_seq(plan_id, msg.sequence_order as i32)
            .await
        {
            return;
        }

        tracing::info!(
            target: "persist",
            sequence_order = msg.sequence_order,
            role = ?msg.message.role,
            is_streaming = msg.is_streaming,
            tool_calls_count = msg.tool_call_entries.len(),
            plan_id = %plan_id,
            "persist_message 被调用"
        );

        let msg_type = determine_msg_type(msg);
        let msg_json = match serde_json::to_string(&msg.message) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("序列化消息失败: {}", e);
                return;
            }
        };
        if let Err(e) = self
            .repo
            .create(plan_id, &msg_json, msg.sequence_order as i32, &msg_type, false)
            .await
        {
            tracing::error!("持久化聊天消息失败: {}", e);
        }
    }

    async fn load_messages(&self, plan_id: &str) -> Vec<ChatMessage> {
        let rows = match self.repo.find_by_plan_id(plan_id).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!("加载聊天消息失败: {}", e);
                return Vec::new();
            }
        };

        let mut result = Vec::with_capacity(rows.len());

        // 第一遍：反序列化所有消息
        for row in &rows {
            let message: Message = match serde_json::from_str(&row.message_json) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("反序列化消息失败 (id={}): {}", row.id, e);
                    continue;
                }
            };

            let tool_call_id = message.tool_call_id.clone();
            let tool_call_entries = build_tool_call_entries(&message);

            result.push(ChatMessage {
                message,
                sequence_order: row.sequence_order as u64,
                is_streaming: false,
                tool_call_id,
                tool_call_entries,
            });
        }

        // 第二遍：将 Tool 消息的 content 按 tool_call_id 关联到 Assistant 的 tool_call_entries
        for i in 0..result.len() {
            match result[i].message.role {
                MessageRole::Tool => {
                    let Some(tool_call_id) = result[i].tool_call_id.clone() else {
                        continue;
                    };
                    let Some(content_text) = result[i].message.content.as_ref().and_then(|c| {
                        if let MessageContent::Text { text } = c { Some(text.as_str()) } else { None }
                    }) else {
                        continue;
                    };
                    let result_value = serde_json::from_str(content_text)
                        .unwrap_or_else(|_| serde_json::Value::String(content_text.to_string()));
                    // 找到携带该 tool_call_id 的 Assistant 消息，回填到对应 entry
                    // （tool_call_id 全局唯一，向前扫描所有 assistant 直到命中）
                    for cm in result.iter_mut().take(i).rev() {
                        if matches!(cm.message.role, MessageRole::Assistant) {
                            if let Some(entry) = cm
                                .tool_call_entries
                                .iter_mut()
                                .find(|e| e.tool_call_id == tool_call_id)
                            {
                                entry.result = Some(result_value);
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        tracing::info!(
            target: "persist",
            plan_id = %plan_id,
            total = rows.len(),
            loaded = result.len(),
            roles = ?result.iter().map(|m| format!("{:?}", m.message.role)).collect::<Vec<_>>(),
            "load_messages"
        );

        result
    }

    async fn delete_messages(&self, plan_id: &str) {
        if let Err(e) = self.repo.delete_by_plan_id(plan_id).await {
            tracing::error!("删除持久化聊天消息失败: {}", e);
        }
    }
}

// ── 内部辅助 ──────────────────────────────────────────────────────────────

/// 根据 `ChatMessage` 内容确定 `msg_type`。
fn determine_msg_type(cm: &ChatMessage) -> &'static str {
    match cm.message.role {
        MessageRole::User => "user",
        MessageRole::Assistant => {
            if cm.message.tool_calls.is_some() && !cm.message.tool_calls.as_ref().unwrap().is_empty()
            {
                "tool_call"
            } else if cm.message.reasoning_content.is_some() {
                "reasoning"
            } else {
                "text"
            }
        }
        MessageRole::Tool => "tool_result",
        _ => "text",
    }
}

/// 从 `Message.tool_calls` 构建 `ToolCallEntry` 列表。
///
/// 加载时无法知道执行结果（is_error），默认标记为 Completed。
fn build_tool_call_entries(message: &Message) -> Vec<ToolCallEntry> {
    let Some(tool_calls) = &message.tool_calls else {
        return Vec::new();
    };

    tool_calls
        .iter()
        .map(|tc| ToolCallEntry {
            tool_call_id: tc.id.clone(),
            phase: ToolCallPhase::Completed,
            result: None,
            is_error: false,
        })
        .collect()
}
