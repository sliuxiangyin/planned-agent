//! PlansFlexibleService 派生 hook：封装 plans_flexible 版本快照的写入与最新版本号查询。

use std::sync::Arc;

use crate::storage::entities::plans_flexible::Model as PlansFlexibleModel;
use crate::storage::error::StorageResult;
use crate::storage::repository::PlansFlexibleRepo;

/// plans_flexible 表服务：在 Repository 之上提供写入与最新版本号查询接口。
#[derive(Clone)]
pub struct PlansFlexibleService {
    repo: Arc<PlansFlexibleRepo>,
}

impl PlansFlexibleService {
    pub fn new(repo: Arc<PlansFlexibleRepo>) -> Self {
        Self { repo }
    }

    /// 写入一条 plans_flexible 快照，返回写入后的完整 Model。
    /// version 由 repo 自动递增（最新版本 +1，首条为 1）。
    /// `session_id` 关联产出该版本的会话；无会话写入（如 plans_flexible 工具）传 None。
    pub async fn write(
        &self,
        plan_id: &str,
        session_id: Option<String>,
        input_schema: &str,
        output: &str,
        steps: &str,
        execution_plan: &str,
    ) -> StorageResult<PlansFlexibleModel> {
        self.repo
            .create(plan_id, session_id, input_schema, output, steps, execution_plan)
            .await
    }

    /// 按 plan_id 查询最新版本号（version 最大者）。该 plan 尚无快照时返回 None。
    pub async fn get_latest_version(&self, plan_id: &str) -> StorageResult<Option<i32>> {
        self.repo.find_latest_version(plan_id).await
    }
}
