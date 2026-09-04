//! plans_flexible 表仓库 — 灵活模式计划版本快照（写入 + 最新版本号查询）。

use chrono::Utc;
use sea_orm::*;
use uuid::Uuid;

use crate::storage::entities::plans_flexible;
use crate::storage::error::{StorageError, StorageResult};

/// plans_flexible 表仓库
pub struct PlansFlexibleRepo {
    db: DatabaseConnection,
}

impl PlansFlexibleRepo {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 写入一条 plans_flexible 快照，返回完整 Model
    ///
    /// 每次调用生成新的 UUID id 与当前时间戳；version 自动递增：
    /// 基于该 plan 最新版本号 +1，首条快照默认 version=1。
    /// `session_id` 必填：快照必然归属某个会话。
    pub async fn create(
        &self,
        plan_id: &str,
        session_id: &str,
        input_schema: &str,
        output: &str,
        steps: &str,
        execution_plan: &str,
    ) -> StorageResult<plans_flexible::Model> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        // version 自动递增：基于该 plan 最新版本 +1，首条默认 1
        let version = match self.find_latest_version(plan_id).await? {
            Some(latest) => latest + 1,
            None => 1,
        };
        let model = plans_flexible::ActiveModel {
            id: Set(id),
            plan_id: Set(plan_id.to_string()),
            session_id: Set(Some(session_id.to_string())),
            version: Set(version),
            input_schema: Set(input_schema.to_string()),
            output: Set(output.to_string()),
            steps: Set(steps.to_string()),
            execution_plan: Set(execution_plan.to_string()),
            created_at: Set(now),
        };
        let res = model.insert(&self.db).await?;
        Ok(res)
    }

    /// 按 plan_id + session_id 查询该会话已落的快照（一个 session 至多一条）。
    /// 用于"同 session 产出 → 覆盖更新（version 不变）"的判断。
    pub async fn find_by_plan_and_session(
        &self,
        plan_id: &str,
        session_id: &str,
    ) -> StorageResult<Option<plans_flexible::Model>> {
        let model = plans_flexible::Entity::find()
            .filter(plans_flexible::Column::PlanId.eq(plan_id))
            .filter(plans_flexible::Column::SessionId.eq(session_id))
            .order_by_desc(plans_flexible::Column::Version)
            .one(&self.db)
            .await?;
        Ok(model)
    }

    /// 覆盖更新一条快照的内容（version / id / session_id / created_at 保持不变）。
    /// 用于同 session 内多次产出的覆盖写入。
    pub async fn update_content(
        &self,
        id: &str,
        input_schema: &str,
        output: &str,
        steps: &str,
        execution_plan: &str,
    ) -> StorageResult<plans_flexible::Model> {
        let mut am: plans_flexible::ActiveModel = plans_flexible::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("plans_flexible '{id}' not found")))?
            .into();
        am.input_schema = Set(input_schema.to_string());
        am.output = Set(output.to_string());
        am.steps = Set(steps.to_string());
        am.execution_plan = Set(execution_plan.to_string());
        let res = am.update(&self.db).await?;
        Ok(res)
    }

    /// 按 plan_id 查询最新版本号（version 最大者）。该 plan 尚无快照时返回 None。
    pub async fn find_latest_version(&self, plan_id: &str) -> StorageResult<Option<i32>> {
        let model = plans_flexible::Entity::find()
            .filter(plans_flexible::Column::PlanId.eq(plan_id))
            .order_by_desc(plans_flexible::Column::Version)
            .one(&self.db)
            .await?;
        Ok(model.map(|m| m.version))
    }
}
