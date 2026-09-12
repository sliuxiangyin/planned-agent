//! plans_flexible_sessions 表仓库 — 「会话即版本」：创建会话 + 会话/版本列表（带搜索）。

use chrono::Utc;
use sea_orm::*;
use uuid::Uuid;

use crate::storage::entities::plans_flexible_sessions;
use crate::storage::error::{StorageError, StorageResult};

/// 会话状态常量
pub mod status {
    /// 进行中 / 未定稿草稿
    pub const ACTIVE: &str = "active";
    /// 已定稿封版
    pub const PRODUCED: &str = "produced";
    /// 中途被弃
    pub const ABANDONED: &str = "abandoned";
}

/// plans_flexible_sessions 表仓库
pub struct PlansFlexibleSessionsRepo {
    db: DatabaseConnection,
}

impl PlansFlexibleSessionsRepo {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 开一个新会话（= 新版本）。
    ///
    /// 生成新的 UUID id 与当前时间戳；`status` 默认 active，`is_default` 默认 false，
    /// 定稿产物四件套留空。`version` 自动分配：取该 plan 现有最大版本号 patch +1，
    /// 首个会话为 `v1.0.0`（保证 plan 内单调递增、唯一）。
    pub async fn create(
        &self,
        plan_id: &str,
        title: &str,
    ) -> StorageResult<plans_flexible_sessions::Model> {
        let now = Utc::now().to_rfc3339();
        let id = Uuid::new_v4().to_string();
        let version = self.next_version(plan_id).await?;
        let model = plans_flexible_sessions::ActiveModel {
            id: Set(id),
            plan_id: Set(plan_id.to_string()),
            title: Set(title.to_string()),
            version: Set(version),
            status: Set(status::ACTIVE.to_string()),
            is_default: Set(false),
            input_schema: Set(None),
            output: Set(None),
            steps: Set(None),
            execution_plan: Set(None),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            closed_at: Set(None),
        };
        let res = model.insert(&self.db).await?;
        Ok(res)
    }

    /// 列出某 plan 下的会话（按创建时间倒序）。
    ///
    /// `search` 非空时按关键词模糊匹配 `title` 或 `version`；为空则返回全部。
    pub async fn list_by_plan(
        &self,
        plan_id: &str,
        search: Option<&str>,
    ) -> StorageResult<Vec<plans_flexible_sessions::Model>> {
        let mut query = plans_flexible_sessions::Entity::find()
            .filter(plans_flexible_sessions::Column::PlanId.eq(plan_id));
        if let Some(keyword) = search.filter(|s| !s.trim().is_empty()) {
            let kw = keyword.trim().to_string();
            query = query.filter(
                plans_flexible_sessions::Column::Title
                    .contains(&kw)
                    .or(plans_flexible_sessions::Column::Version.contains(&kw)),
            );
        }
        let list = query
            .order_by_desc(plans_flexible_sessions::Column::CreatedAt)
            .all(&self.db)
            .await?;
        Ok(list)
    }

    /// 更新会话标题，并刷新 `updated_at`，返回更新后的 Model。
    pub async fn update_title(
        &self,
        id: &str,
        title: &str,
    ) -> StorageResult<plans_flexible_sessions::Model> {
        let now = Utc::now().to_rfc3339();
        let mut am: plans_flexible_sessions::ActiveModel =
            plans_flexible_sessions::Entity::find_by_id(id)
                .one(&self.db)
                .await?
                .ok_or_else(|| {
                    StorageError::NotFound(format!("plans_flexible_sessions '{id}' not found"))
                })?
                .into();
        am.title = Set(title.to_string());
        am.updated_at = Set(now);
        let res = am.update(&self.db).await?;
        Ok(res)
    }

    /// 定稿：把 step5 产出的模板四件套写入指定会话行，置 `status=produced`，
    /// 刷新 `updated_at` 并写 `closed_at`，返回更新后的 Model。
    ///
    /// 目标行由 `id`（= 会话/版本 id）定位；同一会话反复产出即覆盖同一行。
    pub async fn produce(
        &self,
        id: &str,
        input_schema: &str,
        output: &str,
        steps: &str,
        execution_plan: &str,
    ) -> StorageResult<plans_flexible_sessions::Model> {
        let now = Utc::now().to_rfc3339();
        let mut am: plans_flexible_sessions::ActiveModel =
            plans_flexible_sessions::Entity::find_by_id(id)
                .one(&self.db)
                .await?
                .ok_or_else(|| {
                    StorageError::NotFound(format!("plans_flexible_sessions '{id}' not found"))
                })?
                .into();
        am.input_schema = Set(Some(input_schema.to_string()));
        am.output = Set(Some(output.to_string()));
        am.steps = Set(Some(steps.to_string()));
        am.execution_plan = Set(Some(execution_plan.to_string()));
        am.status = Set(status::PRODUCED.to_string());
        am.updated_at = Set(now.clone());
        am.closed_at = Set(Some(now));
        let res = am.update(&self.db).await?;
        Ok(res)
    }

    /// 计算该 plan 的下一个版本号：现有最大版本 patch +1；无会话时返回 `v1.0.0`。
    async fn next_version(&self, plan_id: &str) -> StorageResult<String> {
        let rows = plans_flexible_sessions::Entity::find()
            .filter(plans_flexible_sessions::Column::PlanId.eq(plan_id))
            .all(&self.db)
            .await?;
        let latest = rows
            .iter()
            .map(|m| parse_version(&m.version))
            .max();
        Ok(match latest {
            Some((major, minor, patch)) => format!("v{major}.{minor}.{}", patch + 1),
            None => "v1.0.0".to_string(),
        })
    }
}

/// 解析语义化版本号 `vX.Y.Z` 为 (major, minor, patch)；无法解析的分量按 0 计。
fn parse_version(v: &str) -> (u64, u64, u64) {
    let mut parts = v.trim_start_matches('v').split('.');
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor, patch)
}
