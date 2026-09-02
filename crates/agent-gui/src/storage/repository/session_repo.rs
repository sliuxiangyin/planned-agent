//! sessions 表仓库 — 灵活模式会话（创作过程）生命周期 CRUD。

use chrono::Utc;
use sea_orm::*;
use uuid::Uuid;

use crate::storage::entities::session;
use crate::storage::error::StorageResult;

/// 会话状态常量
pub mod status {
    /// 进行中 / 未定稿草稿
    pub const ACTIVE: &str = "active";
    /// 已定稿封版
    pub const PRODUCED: &str = "produced";
    /// 中途被弃
    pub const ABANDONED: &str = "abandoned";
}

/// sessions 表仓库
pub struct SessionRepo {
    db: DatabaseConnection,
}

impl SessionRepo {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 开一个新会话（默认 active）。可带衍生来源版本与参考注入上下文。
    pub async fn create(
        &self,
        plan_id: &str,
        derived_from_version: Option<i32>,
        reference_context: Option<String>,
    ) -> StorageResult<session::Model> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        let model = session::ActiveModel {
            id: Set(id),
            plan_id: Set(plan_id.to_string()),
            status: Set(status::ACTIVE.to_string()),
            derived_from_version: Set(derived_from_version),
            reference_context: Set(reference_context),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            closed_at: Set(None),
        };
        let res = model.insert(&self.db).await?;
        Ok(res)
    }

    /// 按 id 查找
    pub async fn find_by_id(&self, id: &str) -> StorageResult<Option<session::Model>> {
        let res = session::Entity::find_by_id(id).one(&self.db).await?;
        Ok(res)
    }

    /// 按 plan_id 列出全部会话（按创建时间倒序）
    pub async fn find_by_plan_id(&self, plan_id: &str) -> StorageResult<Vec<session::Model>> {
        let list = session::Entity::find()
            .filter(session::Column::PlanId.eq(plan_id))
            .order_by_desc(session::Column::CreatedAt)
            .all(&self.db)
            .await?;
        Ok(list)
    }

    /// 查找某 plan 的进行中会话（active）
    pub async fn find_active_by_plan_id(
        &self,
        plan_id: &str,
    ) -> StorageResult<Option<session::Model>> {
        let model = session::Entity::find()
            .filter(session::Column::PlanId.eq(plan_id))
            .filter(session::Column::Status.eq(status::ACTIVE))
            .order_by_desc(session::Column::CreatedAt)
            .one(&self.db)
            .await?;
        Ok(model)
    }

    /// 更新会话状态（active → produced / abandoned 等），并写 closed_at。
    pub async fn update_status(&self, id: &str, status_val: &str) -> StorageResult<()> {
        let now = Utc::now().to_rfc3339();
        let closed_at = if status_val == status::ACTIVE {
            None
        } else {
            Some(now.clone())
        };
        session::ActiveModel {
            id: Set(id.to_string()),
            status: Set(status_val.to_string()),
            updated_at: Set(now),
            closed_at: Set(closed_at),
            ..Default::default()
        }
        .update(&self.db)
        .await?;
        Ok(())
    }
}
