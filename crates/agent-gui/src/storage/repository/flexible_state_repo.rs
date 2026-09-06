//! flexible_state 表仓库 — 灵活模式流程中间状态（当前阶段 + 各步骤产物）。
//!
//! 按 plan_id + session_id 定位（一个 session 至多一行），写入走 upsert 语义。

use chrono::Utc;
use sea_orm::*;
use uuid::Uuid;

use crate::storage::entities::flexible_state;
use crate::storage::error::{StorageError, StorageResult};

/// flexible_state 表仓库
pub struct FlexibleStateRepo {
    db: DatabaseConnection,
}

impl FlexibleStateRepo {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 按 plan_id + session_id 查询该会话的状态（一个 session 至多一行）。
    pub async fn find_by_plan_and_session(
        &self,
        plan_id: &str,
        session_id: &str,
    ) -> StorageResult<Option<flexible_state::Model>> {
        let model = flexible_state::Entity::find()
            .filter(flexible_state::Column::PlanId.eq(plan_id))
            .filter(flexible_state::Column::SessionId.eq(session_id))
            .one(&self.db)
            .await?;
        Ok(model)
    }

    /// 插入一条状态记录。
    pub async fn create(
        &self,
        plan_id: &str,
        session_id: &str,
        current_step: &str,
        products: &str,
    ) -> StorageResult<flexible_state::Model> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        let model = flexible_state::ActiveModel {
            id: Set(id),
            plan_id: Set(plan_id.to_string()),
            session_id: Set(session_id.to_string()),
            current_step: Set(current_step.to_string()),
            products: Set(products.to_string()),
            created_at: Set(now.clone()),
            updated_at: Set(now),
        };
        let res = model.insert(&self.db).await?;
        Ok(res)
    }

    /// 覆盖更新一条状态记录的内容（id / plan_id / session_id / created_at 保持不变）。
    pub async fn update_content(
        &self,
        id: &str,
        current_step: &str,
        products: &str,
    ) -> StorageResult<flexible_state::Model> {
        let mut am: flexible_state::ActiveModel = flexible_state::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("flexible_state '{id}' not found")))?
            .into();
        am.current_step = Set(current_step.to_string());
        am.products = Set(products.to_string());
        am.updated_at = Set(Utc::now().to_rfc3339());
        let res = am.update(&self.db).await?;
        Ok(res)
    }
}
