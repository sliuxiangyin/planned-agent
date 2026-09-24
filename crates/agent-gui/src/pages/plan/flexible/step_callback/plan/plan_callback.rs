//! `flexible_plan` 的定稿登记回调：计划定稿后写 `steps` 并推进 `planned`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`
//! （见 `docs/chat-flexible-回调会话归属设计.md`）。
//!
//! 定稿判定（与 `flexible_plan.toml` 的输出契约一致）：`{"status":"planned", ...}` 才算定稿；
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
pub(super) const AGENT: &str = "flexible_plan";
/// 定稿 status：输出顶层 `status` 等于它才算定稿（链组装要拿它建前置分析）。
pub(super) const OK_STATUS: &str = "planned";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "planned";
/// 定稿时要登记的产物 key（值取输出 JSON 中的同名字段）。
///
/// - `steps`：粗粒度步骤骨架（子目标 + 依赖 + 期望产出），供参数化步注入后就地占位。
const PRODUCTS: &[&str] = &["steps"];
/// 定稿时要清除（置 `null`）的下游产物。
///
/// 参数化步产出的 `inputs` 与输出定义步产出的 `output_schema` 都依赖本步的 `steps`：
/// 重跑本步即作废 —— 必须清除。`steps` 本身由本步覆盖写入，不在清除之列。
const CLEAR: &[&str] = &["inputs", "output_schema"];

/// `flexible_plan` 的定稿登记回调。
pub(super) struct PlanCallback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态）。
    service: Arc<PlansFlexibleService>,
}

impl PlanCallback {
    pub(super) fn new(plan_id: String, service: Arc<PlansFlexibleService>) -> Self {
        Self { plan_id, service }
    }
}

#[async_trait]
impl SubAgentResultCallback for PlanCallback {
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

        // 登记 steps，并清除下游的 inputs（重跑计划步 ⇒ 参数化结果作废）
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
            // 写库失败不可重试，如实上报。
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

    /// 本 step 独有的回归价值：**常量取值正确**（登记 steps、清除下游 inputs 与输出契约）。
    /// 公共行为见 `super::super::commit` 的测试，不在此重复。
    #[test]
    fn commits_steps_and_clears_downstream_inputs() {
        let parsed = serde_json::json!({
            "status": "planned",
            "steps": [
                {
                    "result_reference": "#E1",
                    "intent": "读取 /var/log/app.log 内容",
                    "expected_output": "得到 /var/log/app.log 的原始日志内容",
                    "dependencies": []
                }
            ],
        });
        let patch = build_patch(AGENT, &parsed, PRODUCTS, CLEAR);
        let mut keys: Vec<&str> = patch.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["inputs", "output_schema", "steps"]);
        assert!(patch["inputs"].is_null(), "下游 inputs 应被清除");
        assert!(patch["output_schema"].is_null(), "下游输出契约应被清除");
        assert_eq!(patch["steps"][0]["result_reference"], "#E1");
        assert_eq!(NEXT_STEP, "planned");
        assert_eq!(OK_STATUS, "planned");
    }
}
