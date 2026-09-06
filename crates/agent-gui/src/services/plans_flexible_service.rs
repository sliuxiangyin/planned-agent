//! PlansFlexibleService 聚合编排层：协调 plans_flexible / plans / sessions 多表间的灵活计划流程。
//!
//! 负责跨表的语义操作：
//! - 模板版本快照的写入与最新版本号查询（plans_flexible 单表，供工具与 step5 共用）；
//! - 当前会话的定位与新建（sessions × plans.current_session_id）。
//!
//! 后续会话生命周期（定稿落版本、封版开新、翻回历史）继续在此聚合。

use std::sync::Arc;

use crate::storage::entities::plans_flexible::Model as PlansFlexibleModel;
use crate::storage::entities::session;
use crate::storage::error::{StorageError, StorageResult};
use crate::storage::repository::{
    FlexibleStateRepo, PlanRepo, PlansFlexibleRepo, SessionRepo,
};

/// 灵活计划聚合服务：注入相关仓库，向上提供跨表的业务操作。
#[derive(Clone)]
pub struct PlansFlexibleService {
    /// plans_flexible 表（模板版本快照）
    repo: Arc<PlansFlexibleRepo>,
    /// plans 表（当前会话指针、版本指针）
    plan_repo: Arc<PlanRepo>,
    /// sessions 表（会话生命周期）
    session_repo: Arc<SessionRepo>,
    /// flexible_state 表（流程中间状态：当前阶段 + 各步骤产物）
    state_repo: Arc<FlexibleStateRepo>,
}

impl PlansFlexibleService {
    pub fn new(
        repo: Arc<PlansFlexibleRepo>,
        plan_repo: Arc<PlanRepo>,
        session_repo: Arc<SessionRepo>,
        state_repo: Arc<FlexibleStateRepo>,
    ) -> Self {
        Self {
            repo,
            plan_repo,
            session_repo,
            state_repo,
        }
    }

    // ────────────────────────── 会话定位 ──────────────────────────

    /// 获取（必要时新建）当前会话。
    ///
    /// 职责：保证返回一个可用的当前会话。
    /// - `plans.current_session_id` 为空，或它指向的 session 已不存在 → 新建一个 `active` 会话，
    ///   并回写 `plans.current_session_id`；
    /// - 否则返回指针指向的既有会话。
    ///
    /// 计划不存在时返回 `NotFound`。
    pub async fn ensure_current_session(&self, plan_id: &str) -> StorageResult<session::Model> {
        // 计划必须存在，否则无法挂靠会话
        let plan = self
            .plan_repo
            .find_by_id(plan_id)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("plan '{plan_id}' not found")))?;

        // 指针存在且指向的会话仍有效 → 直接返回
        if let Some(sid) = plan.current_session_id.clone() {
            if let Some(existing) = self.session_repo.find_by_id(&sid).await? {
                return Ok(existing);
            }
        }

        // 无指针 / 指针失效 → 新建 active 会话并回写指针
        let created = self.session_repo.create(plan_id, None, None).await?;
        self.plan_repo
            .update_current_session_id(plan_id, Some(created.id.clone()))
            .await?;
        Ok(created)
    }

    // ────────────────────────── 版本快照（plans_flexible）──────────────────────────

    /// 保存某会话产出的模板快照（upsert 语义），返回完整 Model。
    ///
    /// - 该 plan 下该 session 已有快照 → 覆盖该行内容（version 不变，同会话反复产出只更新同一版本）；
    /// - 该 session 尚无快照 → 新增一条（version 为该 plan 最新版本 +1，首条为 1）。
    ///
    /// `session_id` 必填：plans_flexible 快照必然归属某个会话。
    pub async fn save_snapshot(
        &self,
        plan_id: &str,
        session_id: &str,
        input_schema: &str,
        output: &str,
        steps: &str,
        execution_plan: &str,
    ) -> StorageResult<PlansFlexibleModel> {
        if let Some(existing) = self
            .repo
            .find_by_plan_and_session(plan_id, session_id)
            .await?
        {
            self.repo
                .update_content(
                    &existing.id,
                    input_schema,
                    output,
                    steps,
                    execution_plan,
                )
                .await
        } else {
            self.repo
                .create(plan_id, session_id, input_schema, output, steps, execution_plan)
                .await
        }
    }

    /// 按 plan_id 查询最新版本号（version 最大者）。该 plan 尚无快照时返回 None。
    pub async fn get_latest_version(&self, plan_id: &str) -> StorageResult<Option<i32>> {
        self.repo.find_latest_version(plan_id).await
    }

    // ────────────────────────── 流程中间状态（flexible_state）──────────────────────────

    /// 读取某会话的流程中间状态（current_step + products）。该会话尚无记录时返回 None。
    ///
    /// 纯查询，无副作用（不会触发会话新建）。`session_id` 由调用方（协调器工具）从
    /// 当前会话 watch 槽提供，必填。
    pub async fn load_state(
        &self,
        plan_id: &str,
        session_id: &str,
    ) -> StorageResult<Option<(String, String)>> {
        let state = self
            .state_repo
            .find_by_plan_and_session(plan_id, session_id)
            .await?;
        Ok(state.map(|m| (m.current_step, m.products)))
    }

    /// 覆盖保存某会话的流程中间状态（upsert 语义），返回完整内容。
    ///
    /// 该 plan+session 已有记录 → 覆盖（current_step + products）；否则新增一条。
    pub async fn save_state(
        &self,
        plan_id: &str,
        session_id: &str,
        current_step: &str,
        products: &str,
    ) -> StorageResult<(String, String)> {
        if let Some(existing) = self
            .state_repo
            .find_by_plan_and_session(plan_id, session_id)
            .await?
        {
            self.state_repo
                .update_content(&existing.id, current_step, products)
                .await
                .map(|m| (m.current_step, m.products))
        } else {
            self.state_repo
                .create(plan_id, session_id, current_step, products)
                .await
                .map(|m| (m.current_step, m.products))
        }
    }
}
