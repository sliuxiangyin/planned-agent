//! flexible_step2 结果链：执行成功后登记 `executed`，并从会话历史登记 `execution_trace`。
//!
//! # 链上三环（顺序不可颠倒）
//!
//! 1. **默认前置分析**（[`FlexibleStepPrelude`]）：解析输出、定位会话、判定定稿，并定稿对外文本；
//!    解析失败 → 要求重新输出，非定稿 → 直接终止链（后面两环都不执行）。
//! 2. **轨迹提取**（[`Step2ExecutionTraceCallback`]）：从**会话历史**导出真实工具调用，
//!    登记 `execution_trace` —— 不再由模型自述轨迹
//!    （见 `crates/planned-agent/src/chat/trace.rs` 的说明）。
//! 3. **定稿登记**（[`Step2Callback`]）：写 `compressed_context`、清下游产物、推进 `executed`。
//!
//! 顺序不可颠倒的理由：轨迹落库失败会 `Abort`，而那时**状态还没推进**（协调器重跑 step2
//! 即可自愈）；反过来先推进再落轨迹，会留下「已 `executed` 却没有轨迹」的状态，
//! step4/step5 会在缺轨迹的情况下继续，更难发现。至于「没定稿却落了轨迹」，由前置分析
//! 挡住：非定稿它直接终止链，后面两环都不执行。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`
//! （见 `docs/chat-flexible-回调会话归属设计.md`）。
//!
//! 定稿判定（与 `flexible_step2.toml` 的输出契约一致）：`{"status":"success", ...}` 才算定稿；
//! 其它（`status:"error"` / 非 JSON）**不登记任何产物**，保持原阶段（协调器按 prompt 询问重试或取消）。
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

mod execution_trace;
mod tool_trace;

use execution_trace::Step2ExecutionTraceCallback;

/// 子 agent 工具名（日志用）。
const AGENT: &str = "flexible_step2";
/// 定稿 status：输出顶层 `status` 等于它才算定稿。
const OK_STATUS: &str = "success";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "executed";
/// 定稿时要登记的产物 key（值取输出 JSON 中的同名字段）。
///
/// 注意 `execution_trace` 不在这里 —— 它来自会话历史，由 [`Step2ExecutionTraceCallback`] 独立登记。
const PRODUCTS: &[&str] = &["compressed_context"];
/// 定稿时要清除（置 `null`）的下游产物 —— 重跑 step2 ⇒ step3/step4 的定稿作废。
const CLEAR: &[&str] = &["field_selection_result", "parameter_confirmation_result"];

/// `flexible_step2` 的定稿登记回调。
struct Step2Callback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态）。
    service: Arc<PlansFlexibleService>,
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

        // 只登记 compressed_context（轨迹由上一环从会话历史单独落库），并清掉两个下游产物
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

/// 创建 `flexible_step2` 结果链（方便传给 `register_sub_agent`）。
///
/// 三环顺序不可颠倒：轨迹先落库（失败则 `Abort`，状态未推进，可重跑自愈），定稿登记随后
/// 推进 `current_step`；反之会留下「已 `executed` 却没有轨迹」的状态。
pub fn create_step2_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![
        // ② 轨迹提取：从会话历史导出真实工具调用，登记 execution_trace
        Arc::new(Step2ExecutionTraceCallback::new(
            plan_id.clone(),
            service.clone(),
        )),
        // ③ 定稿登记：写 compressed_context、清下游产物、推进 executed
        Arc::new(Step2Callback { plan_id, service }),
    ])
    // ① 前置分析：解析 + 守门 + 定稿对外文本
    .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本 step 独有的回归价值：**常量取值正确**。
    /// 公共行为见 `super::commit` 的测试，不在此重复。
    #[test]
    fn commits_compressed_context_and_clears_downstream_but_never_trace() {
        let parsed = serde_json::json!({
            "status": "success",
            "compressed_context": "已追加一行",
        });
        let patch = build_patch(AGENT, &parsed, PRODUCTS, CLEAR);
        let mut keys: Vec<&str> = patch.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "compressed_context",
                "field_selection_result",
                "parameter_confirmation_result",
            ]
        );
        assert_eq!(patch["compressed_context"], "已追加一行");
        assert!(
            patch.get("execution_trace").is_none(),
            "execution_trace 来自会话历史，不得由输出 JSON 写入"
        );
        assert_eq!(NEXT_STEP, "executed");
        assert_eq!(OK_STATUS, "success");
    }
}
