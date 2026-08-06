//! plans 表仓库 —— 计划 CRUD。

use chrono::Utc;
use sea_orm::*;
use uuid::Uuid;

use crate::storage::entities::plan;
use crate::storage::error::StorageResult;

/// plans 表仓库
pub struct PlanRepo {
    db: DatabaseConnection,
}

impl PlanRepo {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 创建计划，返回完整 Model
    pub async fn create(&self, name: &str, mode: &str) -> StorageResult<plan::Model> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        let model = plan::ActiveModel {
            id: Set(id),
            name: Set(name.to_string()),
            description: Set(String::new()),
            mode: Set(mode.to_string()),
            status: Set("pending_generation".to_string()),
            todos: Set("[]".to_string()),
            created_at: Set(now.clone()),
            updated_at: Set(now),
        };
        let res = model.insert(&self.db).await?;
        Ok(res)
    }

    /// 列出全部计划（按创建时间倒序）
    pub async fn find_all(&self) -> StorageResult<Vec<plan::Model>> {
        let list = plan::Entity::find()
            .order_by_desc(plan::Column::CreatedAt)
            .all(&self.db)
            .await?;
        Ok(list)
    }

    /// 按 id 查找
    pub async fn find_by_id(&self, id: &str) -> StorageResult<Option<plan::Model>> {
        let res = plan::Entity::find_by_id(id).one(&self.db).await?;
        Ok(res)
    }

    /// 更新计划状态
    pub async fn update_status(&self, id: &str, status: &str) -> StorageResult<()> {
        let now = Utc::now().to_rfc3339();
        plan::ActiveModel {
            id: Set(id.to_string()),
            status: Set(status.to_string()),
            updated_at: Set(now),
            ..Default::default()
        }
        .update(&self.db)
        .await?;
        Ok(())
    }

    /// 删除计划（接口预留，暂不调用）
    #[allow(dead_code)]
    pub async fn delete(&self, id: &str) -> StorageResult<()> {
        plan::Entity::delete_by_id(id).exec(&self.db).await?;
        Ok(())
    }
}
