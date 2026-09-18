//! flexible_step3 结果回调：字段选择定稿后登记 `fields_selected`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`。
//! step3 几乎必然挂起（要请用户勾选输出字段），挂起-恢复路径同样会带 `arguments`
//! （见 `docs/chat-flexible-回调会话归属设计.md` §8），故恢复后回调仍能定位会话。
//!
//! 定稿判定（与 `flexible_step3.toml` 的输出契约一致）：
//! - `{"status":"fields_selected", ...}` → 登记 `fields_selected`（写入 `field_selection_result`），
//!   并清除下游产物 `parameter_confirmation_result`。
//! - 其它（`empty_result` / `back_to_execute` / `cancelled` / 非 JSON）→ **不登记**。
//!
//! 本 step 的差异全部写在下面几个常量里；编排用 [`super::commit`] 与
//! [`super::analysis::require_analysis`] 提供的零策略工具，解析与守门在 [`super::prelude`]。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{
    ResultDecision, SubAgentCall, SubAgentResultCallback, SubAgentResultChain,
};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::analysis::require_analysis;
use super::commit::{build_patch, commit_state, hand_off};
use super::prelude::FlexibleStepPrelude;

/// 子 agent 工具名（日志用）。
const AGENT: &str = "flexible_step3";
/// 定稿 status：输出顶层 `status` 等于它才算定稿。
const OK_STATUS: &str = "fields_selected";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "fields_selected";
/// 定稿时要登记的产物 key（值取输出 JSON 中的同名字段）。
const PRODUCTS: &[&str] = &["field_selection_result"];
/// 定稿时要清除（置 `null`）的下游产物 —— 字段变了 ⇒ 参数确认作废。
const CLEAR: &[&str] = &["parameter_confirmation_result"];

/// `flexible_step3` 的定稿登记回调。
struct Step3Callback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态）。
    service: Arc<PlansFlexibleService>,
}

#[async_trait]
impl SubAgentResultCallback for Step3Callback {
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

        // 登记 field_selection_result，并清掉下游的参数确认产物
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
            // 写库失败不可重试，如实上报（见 `super::commit::commit_state` 的说明）
            return ResultDecision::Abort(reason);
        }

        // 对外结果不由这里决定（前置分析已定稿）：只决定要不要把值交给下一位。
        hand_off(call)
    }

    fn name(&self) -> &str {
        AGENT
    }
}

/// 创建 `flexible_step3` 结果链（方便传给 `register_sub_agent`）。
///
/// 链上两环：**前置分析**（解析 + 守门 + 定稿对外文本）+ 本文件内的定稿登记回调。
pub fn create_step3_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![Arc::new(Step3Callback { plan_id, service })])
        .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本 step 独有的回归价值：**常量取值正确**。
    /// 公共行为见 `super::commit` 的测试，不在此重复。
    #[test]
    fn commits_field_selection_and_clears_param_confirmation() {
        let parsed = serde_json::json!({
            "status": "fields_selected",
            "field_selection_result": { "selected_fields": ["a"] },
        });
        let patch = build_patch(AGENT, &parsed, PRODUCTS, CLEAR);
        let mut keys: Vec<&str> = patch.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["field_selection_result", "parameter_confirmation_result"]
        );
        assert_eq!(
            patch["field_selection_result"],
            serde_json::json!({ "selected_fields": ["a"] })
        );
        assert_eq!(
            patch.get("parameter_confirmation_result"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(NEXT_STEP, "fields_selected");
        assert_eq!(OK_STATUS, "fields_selected");
    }
}
