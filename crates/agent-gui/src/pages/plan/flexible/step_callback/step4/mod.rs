//! flexible_step4 结果回调：参数确认定稿后登记 `params_confirmed`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`。
//! 与 step3 同理，step4 请用户勾选参数时也会挂起，恢复路径带 `arguments`。
//!
//! 定稿判定（与 `flexible_step4.toml` 的输出契约一致）：
//! - `{"status":"params_confirmed", ...}` → 登记 `params_confirmed`（写入 `parameter_confirmation_result`）。
//! - 其它（`back_to_step3` / `cancelled` / 非 JSON）→ **不登记**。
//!
//! 本档位已是最高的执行前档位，下游只剩 step5 的模板落库（写 `plans_flexible`，不入 `products`），
//! 故没有需要清除的下游产物（`CLEAR` 为空）。
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
const AGENT: &str = "flexible_step4";
/// 定稿 status：输出顶层 `status` 等于它才算定稿。
const OK_STATUS: &str = "params_confirmed";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "params_confirmed";
/// 定稿时要登记的产物 key（值取输出 JSON 中的同名字段）。
const PRODUCTS: &[&str] = &["parameter_confirmation_result"];

/// `flexible_step4` 的定稿登记回调。
struct Step4Callback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态）。
    service: Arc<PlansFlexibleService>,
}

#[async_trait]
impl SubAgentResultCallback for Step4Callback {
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

        // 只登记 parameter_confirmation_result；本档位无下游产物可清
        let patch = build_patch(AGENT, analysis.parsed, PRODUCTS, &[]);
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

/// 创建 `flexible_step4` 结果链（方便传给 `register_sub_agent`）。
///
/// 链上两环：**前置分析**（解析 + 守门 + 定稿对外文本）+ 本文件内的定稿登记回调。
pub fn create_step4_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![Arc::new(Step4Callback { plan_id, service })])
        .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本 step 独有的回归价值：**常量取值正确**（只登记一个产物、无下游清理）。
    /// 公共行为见 `super::commit` 的测试，不在此重复。
    #[test]
    fn commits_param_confirmation_only() {
        let parsed = serde_json::json!({
            "status": "params_confirmed",
            "parameter_confirmation_result": { "params": { "n": 1 } },
        });
        let patch = build_patch(AGENT, &parsed, PRODUCTS, &[]);
        let keys: Vec<&str> = patch.keys().map(String::as_str).collect();
        assert_eq!(keys, ["parameter_confirmation_result"]);
        assert_eq!(
            patch["parameter_confirmation_result"],
            serde_json::json!({ "params": { "n": 1 } })
        );
        assert_eq!(NEXT_STEP, "params_confirmed");
        assert_eq!(OK_STATUS, "params_confirmed");
    }
}
