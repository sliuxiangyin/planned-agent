//! flexible_step1 结果回调：需求澄清定稿后登记 `task_defined`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`（值由父 agent 原样传入，
//! 语义见 `docs/chat-flexible-回调会话归属设计.md`）。
//!
//! 定稿判定（与 `flexible_step1.toml` 的输出契约一致）：
//! - `{"status":"task_defined", ...}` → 登记 `task_defined`（写入 `task_definition` + `output_format`），
//!   并清除 step2~step4 的全部下游产物。
//! - 其它（`status:"ignored"` / `"cancelled"` / 非 JSON）→ **不登记**（由前置分析拦下，本回调不执行）。
//!
//! 语义注记：本回调登记的是「子 agent 已把需求澄清成一份任务定义」这个**定稿动作**，
//! 不涉及「用户是否认同该需求」——用户不认同就会继续补充 / 修改，协调器重跑 step1 后
//! 回调再登记一次、覆盖旧值，二者不冲突。
//!
//! 本 step 的差异全部写在下面几个常量里（写什么 / 清什么 / 推到哪）；编排用
//! [`super::commit`] 与 [`super::analysis::require_analysis`] 提供的零策略工具，
//! 解析与守门在 [`super::prelude`]。

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
const AGENT: &str = "flexible_step1";
/// 定稿 status：输出顶层 `status` 等于它才算定稿。
const OK_STATUS: &str = "task_defined";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "task_defined";
/// 定稿时要登记的产物 key（值取输出 JSON 中的同名字段）。
const PRODUCTS: &[&str] = &["task_definition", "output_format"];
/// 定稿时要清除（置 `null`）的下游产物 —— 需求变了，后续各阶段的定稿一律作废。
const CLEAR: &[&str] = &[
    "execution_trace",
    "compressed_context",
    "field_selection_result",
    "parameter_confirmation_result",
];

/// `flexible_step1` 的定稿登记回调。
struct Step1Callback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态）。
    service: Arc<PlansFlexibleService>,
}

#[async_trait]
impl SubAgentResultCallback for Step1Callback {
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

        // 本 step 的差异：登记哪些产物（PRODUCTS）、清掉哪些下游产物（CLEAR）、推进到哪（NEXT_STEP）
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

/// 创建 `flexible_step1` 结果链（方便传给 `register_sub_agent`）。
///
/// 链上两环：**前置分析**（解析 + 守门 + 定稿对外文本）+ 本文件内的定稿登记回调。
pub fn create_step1_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![Arc::new(Step1Callback { plan_id, service })])
        .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本 step 独有的回归价值：**常量取值正确**（产物集合 / 清理集合 / 推进档位）。
    /// 公共行为（`null` 跳过、`clear` 覆盖、缺失字段等）见 `super::commit` 的测试，不在此重复。
    #[test]
    fn commits_task_definition_and_clears_all_downstream() {
        let parsed = serde_json::json!({
            "status": "task_defined",
            "task_definition": { "task": "建目录" },
            "output_format": "text",
        });
        let patch = build_patch(AGENT, &parsed, PRODUCTS, CLEAR);
        let mut keys: Vec<&str> = patch.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "compressed_context",
                "execution_trace",
                "field_selection_result",
                "output_format",
                "parameter_confirmation_result",
                "task_definition",
            ]
        );
        assert_eq!(
            patch["task_definition"],
            serde_json::json!({ "task": "建目录" })
        );
        assert_eq!(NEXT_STEP, "task_defined");
        assert_eq!(OK_STATUS, "task_defined");
    }
}
