//! chat_messages 表仓库 — 灵活模式聊天消息 CRUD。

use chrono::Utc;
use sea_orm::*;
use uuid::Uuid;

use crate::storage::entities::chat_message;
use crate::storage::error::StorageResult;

/// chat_messages 表仓库
pub struct ChatMessageRepo {
    db: DatabaseConnection,
}

impl ChatMessageRepo {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 插入一条消息
    pub async fn create(
        &self,
        plan_id: &str,
        message_json: &str,
        sequence_order: i32,
        msg_type: &str,
        is_error: bool,
    ) -> StorageResult<chat_message::Model> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        let model = chat_message::ActiveModel {
            id: Set(id),
            plan_id: Set(plan_id.to_string()),
            message_json: Set(message_json.to_string()),
            sequence_order: Set(sequence_order),
            msg_type: Set(msg_type.to_string()),
            is_error: Set(is_error),
            created_at: Set(now),
        };
        let res = model.insert(&self.db).await?;
        Ok(res)
    }

    /// 按 plan_id 查找全部消息（按 sequence_order 正序）
    pub async fn find_by_plan_id(
        &self,
        plan_id: &str,
    ) -> StorageResult<Vec<chat_message::Model>> {
        let list = chat_message::Entity::find()
            .filter(chat_message::Column::PlanId.eq(plan_id))
            .order_by_asc(chat_message::Column::SequenceOrder)
            .all(&self.db)
            .await?;
        Ok(list)
    }

    /// 按 plan_id 删除全部消息
    pub async fn delete_by_plan_id(&self, plan_id: &str) -> StorageResult<()> {
        chat_message::Entity::delete_many()
            .filter(chat_message::Column::PlanId.eq(plan_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }
}
