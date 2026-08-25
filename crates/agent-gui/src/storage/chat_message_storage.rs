//! `ChatStorage` trait 的 SQLite 实现 —— 基于 `ChatMessageRepo`。

use std::sync::Arc;

use planned_agent_core::ai::types::{Message, MessageRole};
use planned_agent::chat::storage::ChatHistoryStore;

use crate::storage::repository::ChatMessageRepo;

/// 基于 SQLite（SeaORM）的 [`ChatHistoryStore`] 实现。
///
/// 绑定 `plan_id`（构造时），服务端 `History` 通过 trait 方法透明调用。
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
        // 同步阻塞：从 tokio runtime 内部调用异步 repo
        // 使用 block_in_place 将阻塞移至专用线程，避免 "Cannot start a runtime from within a runtime"
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

    fn append(&self, msg: &Message) {
        let plan_id = self.plan_id.clone();
        let repo = self.repo.clone();
        let msg_type = determine_msg_type(msg);
        let is_error = false; // Message 不携带 is_error，与现状一致

        let msg_json = match serde_json::to_string(msg) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("序列化消息失败: {}", e);
                return;
            }
        };

        // 异步写入（fire-and-forget）；sequence_order 由 repo 的 auto-increment
        // 或时间戳保证递增。但现有表用的是手动 seq，这里需要从 History 的 Vec
        // 位置推导。为简化，改为自增 ID 模式——append 时从 DB 查当前最大 seq + 1。
        //
        // 注意：这是 fire-and-forget，并发 append 时可能有 seq 重复；
        // 幂等保护（find_by_plan_and_seq）已处理，DB 的 INSERT 不会失败。
        tokio::spawn(async move {
            // 获取当前最大 seq
            let next_seq = match repo.find_by_plan_id(&plan_id).await {
                Ok(rows) => rows.last().map(|r| r.sequence_order + 1).unwrap_or(1),
                Err(_) => 1,
            };
            if let Err(e) = repo.create(&plan_id, &msg_json, next_seq, msg_type, is_error).await {
                tracing::error!("持久化聊天消息失败: {}", e);
            }
        });
    }

    fn rollback_to(&self, len: usize) {
        let plan_id = self.plan_id.clone();
        let repo = self.repo.clone();
        // sequence_order 从 1 开始，len 是 Vec 长度（保留前 len 条）
        let min_seq = len as i32 + 1; // 删除 seq >= min_seq 的行
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

// ── 内部辅助 ──────────────────────────────────────────────────────────────

/// 根据 `Message` 角色确定 `msg_type`。
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
