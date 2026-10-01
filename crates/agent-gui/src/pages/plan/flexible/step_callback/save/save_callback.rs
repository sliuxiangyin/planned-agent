//! `flexible_save` 的定稿登记回调：保存定稿后把三件套落库并推进 `saved`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`。
//!
//! 定稿判定（与 `flexible_save.toml` 的输出契约一致）：`{"status":"saved", ...}` 才算定稿；
//! 其它（`status:"error"` / 非 JSON）**不登记、不落库**。
//!
//! 职责边界：本回调**直接落库** —— 读 `flexible_state.products` 的完整产物，组装
//! `{ task, inputs, steps, output_schema }` 写入 `plans_flexible_sessions.parameterized_task` 列，
//! 不经协调器 LLM 转抄（转抄会改坏字段）。
//!
//! 组装 + 两道校验（输出契约形态、`${name}` 占位符一致性）在
//! [`super::super::commit::build_payload`]，落库 + 通知左侧面板在
//! [`super::super::commit::persist_template`] —— **与 `flexible_revise` 共用**（修订也要把改动
//! 同步回模板，见 `revise/revise_callback.rs`）。
//!
//! 保存的是**带占位的模板**（模板 / 实例分离）：一个计划可换多套参数值，
//! 运行时由 `${name}` 替换展开。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{ResultDecision, SubAgentCall, SubAgentResultCallback};
use serde_json::{Map, Value};

use crate::pages::plan::shared::session::TemplateNotifier;
use crate::services::plans_flexible_service::PlansFlexibleService;

use super::super::analysis::require_analysis;
use super::super::commit::{commit_state, hand_off, persist_template};

/// 子 agent 工具名（日志用，链组装也要用）。
pub(super) const AGENT: &str = "flexible_save";
/// 定稿 status：输出顶层 `status` 等于它才算定稿。
pub(super) const OK_STATUS: &str = "saved";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "saved";

/// `flexible_save` 的定稿登记回调。
pub(super) struct SaveCallback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态 + 落库）。
    service: Arc<PlansFlexibleService>,
    /// 模板更新通知句柄：落库成功后通知左侧面板重读模板。
    notifier: TemplateNotifier,
}

impl SaveCallback {
    pub(super) fn new(
        plan_id: String,
        service: Arc<PlansFlexibleService>,
        notifier: TemplateNotifier,
    ) -> Self {
        Self {
            plan_id,
            service,
            notifier,
        }
    }
}

#[async_trait]
impl SubAgentResultCallback for SaveCallback {
    async fn on_result(&self, call: &SubAgentCall<'_>) -> ResultDecision {
        tracing::info!(
            "[{}] 子 agent '{}' 完成, tool_call_id={}, content_len={}, is_error={}",
            AGENT,
            call.ctx.agent_name,
            call.ctx.tool_call_id,
            call.text().len(),
            call.result.is_error,
        );

        // ── 取前置分析结论（解析 / 定稿判定 / 会话归属都已由它完成）──
        let analysis = match require_analysis(AGENT, call) {
            Ok(analysis) => analysis,
            Err(decision) => return decision,
        };

        // 读已登记的 task / inputs / steps，组装 payload 后整段落库（不经 LLM 转抄）
        let products = match self
            .service
            .load_state(&self.plan_id, &analysis.session_id)
            .await
        {
            Ok(Some((_, products))) => products,
            Ok(None) => {
                return ResultDecision::Abort(format!(
                    "[{AGENT}] 会话 {} 尚无流程状态记录",
                    analysis.session_id
                ))
            }
            Err(e) => {
                return ResultDecision::Abort(format!("[{AGENT}] 读取流程状态失败: {e}"))
            }
        };

        if let Err(reason) = persist_template(
            &self.service,
            &self.notifier,
            &self.plan_id,
            &analysis.session_id,
            &products,
        )
        .await
        {
            return ResultDecision::Abort(format!("[{AGENT}] {reason}"));
        }

        // 推进 saved（无产物登记，空补丁）
        let patch: Map<String, Value> = Map::new();
        if let Err(reason) = commit_state(
            AGENT,
            &self.service,
            &self.plan_id,
            &analysis.session_id,
            Some(NEXT_STEP),
            &patch,
        )
        .await
        {
            return ResultDecision::Abort(reason);
        }

        hand_off(call)
    }

    fn name(&self) -> &str {
        AGENT
    }
}
