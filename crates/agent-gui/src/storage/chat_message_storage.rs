//! `ChatStorage` trait 的 SQLite 实现 —— 基于 `ChatMessageRepo`。

use std::sync::Arc;

use planned_agent_core::ai::types::{Message, MessageRole};
use planned_agent::chat::storage::ChatHistoryStore;

use crate::storage::repository::ChatMessageRepo;

/// 基于 SQLite（SeaORM）的 [`ChatHistoryStore`] 实现。
pub struct ChatMessageStore {
    repo: Arc<ChatMessageRepo>,
    plan_id: String,
}

impl ChatMessageStore {
    pub fn new(plan_id: String, repo: Arc<ChatMessageRepo>) -> Self {
        Self { repo, plan_id }
    }
}

impl ChatHistoryStore for ChatMessageStore {
    fn load(&self) -> Vec<Message> {
        let plan_id = self.plan_id.clone();
        let repo = self.repo.clone();
        let rows = match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(repo.find_by_plan_id(&plan_id))
        }) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!("加载聊天消息失败: {}", e);
                return Vec::new();
            }
        };

        let mut result = Vec::with_capacity(rows.len());
        for row in &rows {
            match serde_json::from_str::<Message>(&row.message_json) {
                Ok(msg) => result.push(msg),
                Err(e) => {
                    tracing::warn!("反序列化消息失败 (id={}): {}", row.id, e);
                }
            }
        }

        tracing::info!(
            target: "store",
            plan_id = %plan_id,
            total = rows.len(),
            loaded = result.len(),
            "ChatMessageStore::load"
        );
        result
    }

    fn append(&self, msg: &Message) -> String {
        let plan_id = self.plan_id.clone();
        let repo = self.repo.clone();
        let msg_type = determine_msg_type(msg);
        let is_error = false;

        let msg_json = match serde_json::to_string(msg) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("序列化消息失败: {}", e);
                return String::new();
            }
        };

        match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let next_seq = match repo.find_by_plan_id(&plan_id).await {
                    Ok(rows) => rows.last().map(|r| r.sequence_order + 1).unwrap_or(1),
                    Err(_) => 1,
                };
                let row = repo
                    .create(&plan_id, &msg_json, next_seq, msg_type, is_error)
                    .await?;
                Ok::<_, anyhow::Error>(row)
            })
        }) {
            Ok(row) => {
                tracing::info!(
                    target: "store",
                    plan_id = %plan_id,
                    msg_id = %row.id,
                    "ChatMessageStore::append"
                );
                row.id
            }
            Err(e) => {
                tracing::error!("持久化聊天消息失败: {}", e);
                String::new()
            }
        }
    }

    fn update(&self, id: &str, msg: &Message) {
        let _plan_id = self.plan_id.clone();
        let repo = self.repo.clone();
        let msg_type = determine_msg_type(msg);
        let id_owned = id.to_string();

        let msg_json = match serde_json::to_string(msg) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("序列化消息失败: {}", e);
                return;
            }
        };

        tokio::spawn(async move {
            if let Err(e) = repo.update_by_id(&id_owned, &msg_json, msg_type).await {
                tracing::error!("更新聊天消息失败 (id={}): {}", id_owned, e);
            }
        });
    }

    fn rollback_to(&self, len: usize) {
        let plan_id = self.plan_id.clone();
        let repo = self.repo.clone();
        let min_seq = len as i32 + 1;
        tokio::spawn(async move {
            if let Err(e) = repo.delete_by_plan_and_gte_seq(&plan_id, min_seq).await {
                tracing::error!("回滚聊天消息失败: {}", e);
            }
        });
    }

    fn clear(&self) {
        let plan_id = self.plan_id.clone();
        let repo = self.repo.clone();
        tokio::spawn(async move {
            if let Err(e) = repo.delete_by_plan_id(&plan_id).await {
                tracing::error!("清空聊天消息失败: {}", e);
            }
        });
    }
}

fn determine_msg_type(msg: &Message) -> &'static str {
    match msg.role {
        MessageRole::User => "user",
        MessageRole::Assistant => {
            if msg.tool_calls.is_some() && !msg.tool_calls.as_ref().unwrap().is_empty() {
                "tool_call"
            } else if msg.reasoning_content.is_some() {
                "reasoning"
            } else {
                "text"
            }
        }
        MessageRole::Tool => "tool_result",
        _ => "text",
    }
}
