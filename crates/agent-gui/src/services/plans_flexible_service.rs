//! PlansFlexibleService 聚合编排层：协调 plans_flexible_sessions 与 flexible_state 表间的灵活计划流程。
//!
//! 负责跨表的语义操作：
//! - flexible_save 定稿产物写入会话行（plans_flexible_sessions，置 status=produced）；
//! - 流程中间状态的读写（flexible_state）。
//!
//! 后续会话生命周期（封版开新、翻回历史）继续在此聚合。

use std::sync::Arc;

use crate::storage::entities::plans_flexible_sessions::Model as PlansFlexibleSessionsModel;
use crate::storage::error::StorageResult;
use crate::storage::repository::{FlexibleStateRepo, PlansFlexibleSessionsRepo};

/// 灵活计划聚合服务：注入相关仓库，向上提供跨表的业务操作。
#[derive(Clone)]
pub struct PlansFlexibleService {
    /// plans_flexible_sessions 表（会话即版本：flexible_save 定稿产物落该会话行）
    sessions_repo: Arc<PlansFlexibleSessionsRepo>,
    /// flexible_state 表（流程中间状态：当前阶段 + 各步骤产物）
    state_repo: Arc<FlexibleStateRepo>,
}

impl PlansFlexibleService {
    pub fn new(
        sessions_repo: Arc<PlansFlexibleSessionsRepo>,
        state_repo: Arc<FlexibleStateRepo>,
    ) -> Self {
        Self {
            sessions_repo,
            state_repo,
        }
    }

    // ─────────────────── 定稿产物（plans_flexible_sessions）───────────────────

    /// 保存某会话定稿产出的参数提取结果（整段 JSON），返回更新后的会话 Model。
    ///
    /// 写入 `session_id` 对应的会话行并置 `status=produced`；同一会话反复产出即覆盖同一行。
    /// `session_id` 必填：产物必然归属某个会话。
    pub async fn save_snapshot(
        &self,
        _plan_id: &str,
        session_id: &str,
        parameterized_task: &str,
    ) -> StorageResult<PlansFlexibleSessionsModel> {
        self.sessions_repo
            .produce(session_id, parameterized_task)
            .await
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

    /// 读-改-写**合并**保存某会话的流程中间状态（与 `flexible_state` 工具的 `save` 同语义）。
    ///
    /// - `current_step`：`Some` 时推进阶段；`None` 保留原值。
    /// - `patch`：产物补丁 —— key 出现即写入；值为 `null` 表示**删除**该产物；未出现的 key 保留原值。
    ///
    /// 供**代码侧**调用方（如子 agent 完成回调）使用。直接用 [`Self::save_state`] 是**全量覆盖**，
    /// 会把上游产物（如 `task_definition`）冲掉，故代码侧一律走本方法。
    pub async fn merge_state(
        &self,
        plan_id: &str,
        session_id: &str,
        current_step: Option<&str>,
        patch: &serde_json::Map<String, serde_json::Value>,
    ) -> StorageResult<(String, String)> {
        let existing = self.load_state(plan_id, session_id).await?;
        let (mut step, mut products) = match existing {
            Some((step, products)) => {
                let parsed = serde_json::from_str::<serde_json::Value>(&products)
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                (step, parsed)
            }
            None => ("none".to_string(), serde_json::Map::new()),
        };

        if let Some(next) = current_step {
            step = next.to_string();
        }
        for (key, value) in patch {
            if value.is_null() {
                products.remove(key);
            } else {
                products.insert(key.clone(), value.clone());
            }
        }

        let products_str = serde_json::Value::Object(products).to_string();
        self.save_state(plan_id, session_id, &step, &products_str)
            .await
    }
}
