//! plans_flexible 表仓库 —— 灵活模式计划版本快照 CRUD。

use chrono::Utc;
use sea_orm::*;
use uuid::Uuid;

use crate::storage::entities::plans_flexible;
use crate::storage::error::StorageResult;

/// plans_flexible 表仓库
pub struct PlanFlexibleRepo {
    db: DatabaseConnection,
}

impl PlanFlexibleRepo {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 创建新版本快照，返回完整 Model
    pub async fn create(
        &self,
        plan_id: &str,
        version: i32,
        previous_summary: &str,
        todos: &str,
        params: &str,
        output_schema: &str,
        input_schema: &str,
    ) -> StorageResult<plans_flexible::Model> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        let model = plans_flexible::ActiveModel {
            id: Set(id),
            plan_id: Set(plan_id.to_string()),
            version: Set(version),
            previous_summary: Set(previous_summary.to_string()),
            todos: Set(todos.to_string()),
            params: Set(params.to_string()),
            output_schema: Set(output_schema.to_string()),
            input_schema: Set(input_schema.to_string()),
            created_at: Set(now),
        };
        let res = model.insert(&self.db).await?;
        Ok(res)
    }

    /// 按 plan_id + version 精确查找
    pub async fn find_by_version(
        &self,
        plan_id: &str,
        version: i32,
    ) -> StorageResult<Option<plans_flexible::Model>> {
        let res = plans_flexible::Entity::find()
            .filter(plans_flexible::Column::PlanId.eq(plan_id))
            .filter(plans_flexible::Column::Version.eq(version))
            .one(&self.db)
            .await?;
        Ok(res)
    }

    /// 获取最新版本（MAX version）
    pub async fn find_latest(
        &self,
        plan_id: &str,
    ) -> StorageResult<Option<plans_flexible::Model>> {
        let res = plans_flexible::Entity::find()
            .filter(plans_flexible::Column::PlanId.eq(plan_id))
            .order_by_desc(plans_flexible::Column::Version)
            .one(&self.db)
            .await?;
        Ok(res)
    }

    /// 列出某个计划的所有版本（按版本号倒序）
    pub async fn list_versions(
        &self,
        plan_id: &str,
    ) -> StorageResult<Vec<plans_flexible::Model>> {
        let list = plans_flexible::Entity::find()
            .filter(plans_flexible::Column::PlanId.eq(plan_id))
            .order_by_desc(plans_flexible::Column::Version)
            .all(&self.db)
            .await?;
        Ok(list)
    }

    /// 获取某个计划的下一个版本号（MAX version + 1）
    pub async fn next_version(&self, plan_id: &str) -> StorageResult<i32> {
        let latest = self.find_latest(plan_id).await?;
        Ok(latest.map(|m| m.version).unwrap_or(0) + 1)
    }

    /// 按 plan_id 删除全部版本快照
    #[allow(dead_code)]
    pub async fn delete_by_plan_id(&self, plan_id: &str) -> StorageResult<()> {
        plans_flexible::Entity::delete_many()
            .filter(plans_flexible::Column::PlanId.eq(plan_id))
            .exec(&self.db)
            .await?;
        Ok(())
    }
}
