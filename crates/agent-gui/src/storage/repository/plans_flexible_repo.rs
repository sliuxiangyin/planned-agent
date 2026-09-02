//! plans_flexible 表仓库 — 灵活模式计划版本快照（写入 + 最新版本号查询）。

use chrono::Utc;
use sea_orm::*;
use uuid::Uuid;

use crate::storage::entities::plans_flexible;
use crate::storage::error::StorageResult;

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
    pub async fn create(
        &self,
        plan_id: &str,
        session_id: Option<String>,
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
            session_id: Set(session_id),
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
