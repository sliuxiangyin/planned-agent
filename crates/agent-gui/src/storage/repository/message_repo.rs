//! messages 表仓库 —— 对话消息 CRUD。

use chrono::Utc;
use sea_orm::*;
use uuid::Uuid;

use crate::storage::entities::message;
use crate::storage::error::StorageResult;

/// messages 表仓库
pub struct MessageRepo {
    db: DatabaseConnection,
}

impl MessageRepo {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 插入一条消息，返回完整 Model
    pub async fn create(
        &self,
        plan_id: &str,
        role: &str,
        content: &str,
    ) -> StorageResult<message::Model> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        let model = message::ActiveModel {
            id: Set(id),
            plan_id: Set(plan_id.to_string()),
            role: Set(role.to_string()),
            content: Set(content.to_string()),
            created_at: Set(now),
        };
        let res = model.insert(&self.db).await?;
        Ok(res)
    }

    /// 按 plan_id 查找全部消息（按创建时间正序）
    pub async fn find_by_plan_id(&self, plan_id: &str) -> StorageResult<Vec<message::Model>> {
        let list = message::Entity::find()
            .filter(message::Column::PlanId.eq(plan_id))
            .order_by_asc(message::Column::CreatedAt)
            .all(&self.db)
            .await?;
        Ok(list)
    }

    /// 更新某计划最后一条 assistant 消息的内容（状态块追加完成后同步持久化）。
    ///
    /// 找不到记录时返回 `Ok(false)`，调用方可按需重试。
    pub async fn update_last_assistant(
        &self,
        plan_id: &str,
        content: &str,
    ) -> StorageResult<bool> {
        let list = message::Entity::find()
            .filter(message::Column::PlanId.eq(plan_id))
            .filter(message::Column::Role.eq("assistant"))
            .order_by_asc(message::Column::CreatedAt)
            .all(&self.db)
            .await?;
        let Some(last) = list.into_iter().last() else {
            return Ok(false);
        };
        message::ActiveModel {
            id: Set(last.id),
            content: Set(content.to_string()),
            ..Default::default()
        }
        .update(&self.db)
        .await?;
        Ok(true)
    }

    /// 按 plan_id 删除全部消息
    pub async fn delete_by_plan_id(&self, plan_id: &str) -> StorageResult<()> {
        message::Entity::delete_many()
            .filter(message::Column::PlanId.eq(plan_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }
}
