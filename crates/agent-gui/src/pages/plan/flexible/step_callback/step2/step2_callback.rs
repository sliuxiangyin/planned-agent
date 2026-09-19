//! `flexible_step2` 的定稿登记回调：参数提取成功后写 `parameterized_task` 并推进 `parameterized`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`
//! （见 `docs/chat-flexible-回调会话归属设计.md`）。
//!
//! 定稿判定（与 `flexible_step2.toml` 的输出契约一致）：`{"status":"success", ...}` 才算定稿；
//! 其它（`status:"error"` / 非 JSON）**不登记任何产物**，保持原阶段（协调器按 prompt 询问重试或取消）。
//!
//! 本 step 的差异全部写在下面几个常量里；编排用 [`super::super::commit`] 与
//! [`super::super::analysis::require_analysis`] 提供的零策略工具，解析与守门在 [`super::super::prelude`]。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{ResultDecision, SubAgentCall, SubAgentResultCallback};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::super::analysis::require_analysis;
use super::super::commit::{build_patch, commit_state, hand_off};

/// 子 agent 工具名（日志用，链组装与轨迹回调也要用）。
pub(super) const AGENT: &str = "flexible_step2";
/// 定稿 status：输出顶层 `status` 等于它才算定稿（链组装要拿它建前置分析）。
pub(super) const OK_STATUS: &str = "success";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "parameterized";
/// 定稿时要登记的产物 key（值取输出 JSON 中的同名字段）。
///
/// - `parameterized_task`：参数提取结果（占位符模板 + 参数表），供 `flexible_save` 落库。
const PRODUCTS: &[&str] = &["parameterized_task"];
/// 定稿时要清除（置 `null`）的下游产物 —— 本 step 之后无 flexible_state 下游产物。
const CLEAR: &[&str] = &[];

/// `flexible_step2` 的定稿登记回调。
pub(super) struct Step2Callback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态）。
    service: Arc<PlansFlexibleService>,
}

impl Step2Callback {
    pub(super) fn new(plan_id: String, service: Arc<PlansFlexibleService>) -> Self {
        Self { plan_id, service }
    }
}

#[async_trait]
impl SubAgentResultCallback for Step2Callback {
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

        // 登记 parameterized_task（无下游产物需清）
        let patch = build_patch(AGENT, analysis.parsed, PRODUCTS, CLEAR);
        if let Err(reason) = commit_state(
            AGENT,
            &self.service,
            &self.plan_id,
            analysis.session_id,
            Some(NEXT_STEP),
            &patch,
        )
        .await
        {
            // 写库失败不可重试，如实上报。此刻上一环可能已写入新轨迹，留下
            // 「新轨迹 + 未推进的 current_step」的中间态 —— 协调器重跑 step2 时会覆盖，无需补偿。
            return ResultDecision::Abort(reason);
        }

        // 本环是链尾：对外结果已由前置分析定稿，直接交接。
        hand_off(call)
    }

    fn name(&self) -> &str {
        AGENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本 step 独有的回归价值：**常量取值正确**（登记 parameterized_task、无下游清除）。
    /// 公共行为见 `super::super::commit` 的测试，不在此重复。
    #[test]
    fn commits_parameterized_task_and_clears_nothing() {
        let parsed = serde_json::json!({
            "status": "success",
            "parameterized_task": {
                "template": "在 ${filepath} 维护日志",
                "parameters": [{ "name": "filepath", "default": "C:/a/b/text.txt" }],
            },
        });
        let patch = build_patch(AGENT, &parsed, PRODUCTS, CLEAR);
        let mut keys: Vec<&str> = patch.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["parameterized_task"]);
        assert_eq!(patch["parameterized_task"]["template"], "在 ${filepath} 维护日志");
        assert_eq!(NEXT_STEP, "parameterized");
        assert_eq!(OK_STATUS, "success");
    }
}
