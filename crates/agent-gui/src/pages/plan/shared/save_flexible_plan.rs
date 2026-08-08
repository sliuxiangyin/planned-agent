//! 灵活模式计划保存：从执行轨迹提炼 CoarseGrainedPlan + 推断 output_schema，
//! 写入 plans_flexible 版本快照，更新 plans 表状态。

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::ChatService;
use planned_agent_prompt_manager::FilePromptManager;

use crate::storage::repository::{MessageRepo, PlanFlexibleRepo, PlanRepo};

use super::super::states::{PlanState, WorkflowState};
use super::super::types::ParamDef;

/// 灵活模式：从轨迹提炼粗粒度计划并保存到 `plans_flexible`。
///
/// 与旧版 `page.rs::save_flexible_plan` 的关键差异：
/// - 不再操作 messages 表（不追加状态块、不持久化 assistant 消息）
/// - 新增 output_schema 推断并持久化
/// - 返回保存后的版本号，调用方可用来更新 UI
pub async fn save_flexible_plan(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    pid: String,
    summary: String,
    params: Vec<ParamDef>,
    plan_repo: Arc<PlanRepo>,
    flex_repo: Arc<PlanFlexibleRepo>,
    mut plan: PlanState,
    mut workflow: WorkflowState,
) {
    // 1. 提炼 CoarseGrainedPlan + output_schema + input_schema
    let (todos_json, output_schema, input_schema) =
        match chat_svc.generate_coarse_plan_from_trace(&summary).await {
            Ok(triple) => triple,
            Err(e) => {
                tracing::error!("从轨迹提炼粗粒度计划失败: {}", e);
                workflow.set_phase(super::super::types::WorkflowPhase::Idle);
                return;
            }
        };

    // 2. 获取下一个版本号
    let version = match flex_repo.next_version(&pid).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("获取版本号失败: {}", e);
            workflow.set_phase(super::super::types::WorkflowPhase::Idle);
            return;
        }
    };

    // 3. 序列化 params
    let params_json = serde_json::to_string(&params).unwrap_or_else(|_| "[]".to_string());

    // 4. 保存到 plans_flexible（best-effort）
    if let Err(e) = flex_repo
        .create(
            &pid,
            version,
            &summary,
            &todos_json,
            &params_json,
            &output_schema,
            &input_schema,
        )
        .await
    {
        tracing::error!("保存灵活计划快照失败: {}", e);
    }

    // 5. 更新 plans.flexible_version（best-effort）
    if let Err(e) = plan_repo.update_flexible_version(&pid, version).await {
        tracing::error!("更新灵活计划版本失败: {}", e);
    }

    // 6. 更新计划状态（best-effort）
    if let Err(e) = plan_repo.update_status(&pid, "generated").await {
        tracing::error!("更新计划状态失败: {}", e);
    }

    tracing::info!(
        "灵活计划 v{} 已保存: plan_id={}, output_schema={}, input_schema={}",
        version,
        pid,
        if output_schema.is_empty() {
            "(未推断)"
        } else {
            &output_schema[..output_schema.len().min(60)]
        },
        if input_schema.is_empty() {
            "(无输入参数)"
        } else {
            &input_schema[..input_schema.len().min(60)]
        }
    );

    // 7. 更新状态信号
    plan.bump_version();
    plan.plan_info.with_mut(|info| {
        if let Some(ref mut i) = *info {
            i.status = "generated".to_string();
        }
    });
    workflow.set_phase(super::super::types::WorkflowPhase::Idle);
}
