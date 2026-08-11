//! 灵活模式计划保存：从完整需求提炼 CoarseGrainedPlan + 推断 output_schema，
//! 写入 plans_flexible 版本快照，更新 plans 表状态。

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::ChatService;
use planned_agent_prompt_manager::FilePromptManager;

use crate::storage::repository::{MessageRepo, PlanFlexibleRepo, PlanRepo};

use super::super::states::{PlanState, WorkflowState};
use super::super::types::{ParamDef, WorkflowPhase};

/// 将字符串安全截断到不超过 `max_bytes` 字节（保证不会切到多字节 UTF-8 字符中间）。
/// 仅用于日志展示，截断长度是"字节上限"，实际返回长度可能略短。
fn truncate_for_log(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        s
    } else {
        &s[..s.floor_char_boundary(max_bytes)]
    }
}

/// 灵活模式：从完整需求提炼粗粒度计划并保存到 `plans_flexible`。
///
/// 与旧版 `page.rs::save_flexible_plan` 的关键差异：
/// - 不再操作 messages 表（不追加状态块、不持久化 assistant 消息）
/// - 新增 output_schema 推断并持久化
/// - 返回保存后的版本号，调用方可用来更新 UI
pub async fn save_flexible_plan(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    pid: String,
    summary: String,
    confirmed_output_schema: String,
    params: Vec<ParamDef>,
    plan_repo: Arc<PlanRepo>,
    flex_repo: Arc<PlanFlexibleRepo>,
    mut plan: PlanState,
    mut workflow: WorkflowState,
) {
    // 1. 提炼 CoarseGrainedPlan + output_schema + input_schema（失败自动重试 1 次）
    let hint = if confirmed_output_schema.is_empty() {
        None
    } else {
        Some(confirmed_output_schema.as_str())
    };
    let mut attempt = 0;
    let triple = loop {
        attempt += 1;
        match chat_svc
            .generate_coarse_plan_from_trace(&summary, hint)
            .await
        {
            Ok(triple) => break Ok(triple),
            Err(e) if attempt < 2 => {
                tracing::warn!("从轨迹提炼粗粒度计划失败（第 {} 次，将重试）: {}", attempt, e);
            }
            Err(e) => break Err(e),
        }
    };
    let (todos_json, output_schema, input_schema) = match triple {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("从轨迹提炼粗粒度计划失败: {}", e);
            workflow.set_phase(super::super::types::WorkflowPhase::Idle);
            // 失败可见：写入阶段输出，用户能在时间轴直接看到失败原因（不再静默白跑）
            workflow.set_phase_output(format!("固化计划失败：{}", e));
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
            truncate_for_log(&output_schema, 60)
        },
        if input_schema.is_empty() {
            "(无输入参数)"
        } else {
            truncate_for_log(&input_schema, 60)
        }
    );

    // 7. 记录固化计划详情，供时间轴"固化计划"卡片 Done 后展开查看
    let mut solidify_detail = format!("已固化执行计划 v{}", version);
    if !todos_json.is_empty() {
        solidify_detail.push_str(&format!("\n\n## 执行计划（CoarseGrainedPlan）\n{}", todos_json));
    }
    if !output_schema.is_empty() {
        solidify_detail.push_str(&format!("\n\n## 输出格式\n{}", output_schema));
    }
    if !input_schema.is_empty() {
        solidify_detail.push_str(&format!("\n\n## 输入参数定义\n{}", input_schema));
    }
    workflow.set_stage_output(WorkflowPhase::Solidifying, solidify_detail);

    // 8. 更新状态信号
    plan.bump_version();
    plan.plan_info.with_mut(|info| {
        if let Some(ref mut i) = *info {
            i.status = "generated".to_string();
        }
    });
    workflow.set_phase(super::super::types::WorkflowPhase::Idle);
}
