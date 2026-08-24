//! 加载计划数据：元数据。

use std::sync::Arc;

use crate::storage::repository::PlanRepo;

use super::super::types::PlanInfo;

/// 从 DB 异步加载计划元数据。
pub async fn load_plan_data(
    pid: String,
    plan_repo: Arc<PlanRepo>,
) -> Option<PlanInfo> {
    if let Ok(Some(plan_model)) = plan_repo.find_by_id(&pid).await {
        tracing::info!(
            "load_plan_data: 加载计划 '{}', mode='{}', status='{}'",
            plan_model.name,
            plan_model.mode,
            plan_model.status,
        );
        Some(PlanInfo {
            name: plan_model.name,
            mode: plan_model.mode,
            status: plan_model.status,
        })
    } else {
        None
    }
}
