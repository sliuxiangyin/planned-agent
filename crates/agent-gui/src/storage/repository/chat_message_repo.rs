//! chat_messages 表仓库 — 灵活模式聊天消息 CRUD。

use chrono::Utc;
use sea_orm::{prelude::*, *};
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
        session_id: Option<String>,
        message_json: &str,
        sequence_order: i32,
        is_error_type: i32,
        is_agent_tool: bool,
    ) -> StorageResult<chat_message::Model> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        let model = chat_message::ActiveModel {
            id: Set(id),
            plan_id: Set(plan_id.to_string()),
            session_id: Set(session_id),
            message_json: Set(message_json.to_string()),
            sequence_order: Set(sequence_order),
            is_error_type: Set(is_error_type),
            is_agent_tool: Set(is_agent_tool),
            created_at: Set(now),
        };
        let res = model.insert(&self.db).await?;
        Ok(res)
    }

    /// 按 plan_id + sequence_order 查找（用于持久化幂等去重）
    pub async fn find_by_plan_and_seq(
        &self,
        plan_id: &str,
        sequence_order: i32,
    ) -> StorageResult<Option<chat_message::Model>> {
        let model = chat_message::Entity::find()
            .filter(chat_message::Column::PlanId.eq(plan_id))
            .filter(chat_message::Column::SequenceOrder.eq(sequence_order))
            .one(&self.db)
            .await?;
        Ok(model)
    }

    /// 删除 plan_id 下 sequence_order >= min_seq 的全部消息（回滚用）。
    pub async fn delete_by_plan_and_gte_seq(
        &self,
        plan_id: &str,
        min_seq: i32,
    ) -> StorageResult<()> {
        chat_message::Entity::delete_many()
            .filter(chat_message::Column::PlanId.eq(plan_id))
            .filter(chat_message::Column::SequenceOrder.gte(min_seq))
            .exec(&self.db)
            .await?;
        Ok(())
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

    /// 按 plan_id + session_id 查找某会话的全部消息（按 sequence_order 正序）
    pub async fn find_by_plan_and_session(
        &self,
        plan_id: &str,
        session_id: &str,
    ) -> StorageResult<Vec<chat_message::Model>> {
        let list = chat_message::Entity::find()
            .filter(chat_message::Column::PlanId.eq(plan_id))
            .filter(chat_message::Column::SessionId.eq(session_id))
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

    /// 按 plan_id + session_id 删除某会话的全部消息
    pub async fn delete_by_plan_and_session(
        &self,
        plan_id: &str,
        session_id: &str,
    ) -> StorageResult<()> {
        chat_message::Entity::delete_many()
            .filter(chat_message::Column::PlanId.eq(plan_id))
            .filter(chat_message::Column::SessionId.eq(session_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    /// 根据 UUID id 更新消息内容
    pub async fn update_by_id(
        &self,
        id: &str,
        message_json: &str,
    ) -> StorageResult<()> {
        chat_message::Entity::update_many()
            .filter(chat_message::Column::Id.eq(id))
            .col_expr(
                chat_message::Column::MessageJson,
                Expr::val(message_json),
            )
            .exec(&self.db)
            .await?;
        Ok(())
    }

    /// 根据 UUID id 删除单条消息
    pub async fn delete_by_id(&self, id: &str) -> StorageResult<()> {
        chat_message::Entity::delete_many()
            .filter(chat_message::Column::Id.eq(id))
            .exec(&self.db)
            .await?;
        Ok(())
    }
}
