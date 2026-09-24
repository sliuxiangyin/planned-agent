//! `flexible_output` 的定稿登记回调：输出契约定稿后写 `output_schema`，并推进 `output_defined`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`（值由父 agent 原样传入）。
//!
//! 定稿判定（与 `flexible_output.toml` 的输出契约一致）：`{"status":"success", ...}` 才算定稿。
//! - 带 `output_schema` 对象 → 登记该产物并推进 `output_defined`。
//! - `output_schema: null`（用户选「现在还定不了」）→ **不登记产物**（`build_patch` 把 `null`
//!   当「跳过写入」，见 [`super::super::commit`]），但**仍推进档位**：落库时该字段即 `null`，
//!   与「整步跳过」等价（见 `docs/planned-agent/flexible-output-step.md` §5）。
//! - 其它（`status:"error"` / 非 JSON）→ **不登记**（由前置分析拦下，本回调不执行）。
//!
//! 本 step 的差异全部写在下面几个常量里；编排用 [`super::super::commit`] 与
//! [`super::super::analysis::require_analysis`] 提供的零策略工具，解析与守门在 [`super::super::prelude`]。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{ResultDecision, SubAgentCall, SubAgentResultCallback};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::super::analysis::require_analysis;
use super::super::commit::{build_patch, commit_state, hand_off};

/// 子 agent 工具名（日志用，链组装也要用）。
pub(super) const AGENT: &str = "flexible_output";
/// 定稿 status：输出顶层 `status` 等于它才算定稿（链组装要拿它建前置分析）。
pub(super) const OK_STATUS: &str = "success";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "output_defined";
/// 定稿时要登记的产物 key（值取输出 JSON 中的同名字段；为 `null` 时跳过写入）。
const PRODUCTS: &[&str] = &["output_schema"];
/// 定稿时要清除（置 `null`）的下游产物 —— 本 step 下游只有 save，而 save 不产 state 产物。
const CLEAR: &[&str] = &[];

/// `flexible_output` 的定稿登记回调。
pub(super) struct OutputCallback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态）。
    service: Arc<PlansFlexibleService>,
}

impl OutputCallback {
    pub(super) fn new(plan_id: String, service: Arc<PlansFlexibleService>) -> Self {
        Self { plan_id, service }
    }
}

#[async_trait]
impl SubAgentResultCallback for OutputCallback {
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

        // 本 step 的差异：登记 output_schema（null 即用户「定不了」，跳过写入但仍推进档位）
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
            // 写库失败不可重试，如实上报（见 `super::super::commit::commit_state` 的说明）
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

    /// 本 step 独有的回归价值：**常量取值正确**（登记 `output_schema`、推进 `output_defined`）。
    /// 公共行为（`null` 跳过、`clear` 覆盖、缺失字段等）见 `super::super::commit` 的测试，不在此重复。
    #[test]
    fn commits_output_schema_and_advances_to_output_defined() {
        let parsed = serde_json::json!({
            "status": "success",
            "output_schema": {
                "kind": "csv",
                "description": "从商品页抽取商品清单",
                "detail": "UTF-8 CSV，首行表头",
                "required": ["title"],
                "wanted": ["stock"],
            },
        });
        let patch = build_patch(AGENT, &parsed, PRODUCTS, CLEAR);
        assert_eq!(patch.len(), 1);
        assert_eq!(patch["output_schema"]["kind"], "csv");
        assert_eq!(patch["output_schema"]["required"][0], "title");
        assert_eq!(patch["output_schema"]["wanted"][0], "stock");
        assert_eq!(NEXT_STEP, "output_defined");
        assert_eq!(OK_STATUS, "success");
    }

    /// 用户选「现在还定不了」= `output_schema: null`：**不登记产物**（`null` 在 `merge_state` 里
    /// 表示删除，写进去反而会清掉已有值），但档位照样推进 —— 落库得到 `null`，与整步跳过等价。
    #[test]
    fn null_schema_registers_nothing() {
        let parsed = serde_json::json!({ "status": "success", "output_schema": null });
        let patch = build_patch(AGENT, &parsed, PRODUCTS, CLEAR);
        assert!(patch.is_empty(), "null 不得登记产物");
        assert!(CLEAR.is_empty(), "本 step 之后无 flexible_state 下游产物需清");
    }
}
