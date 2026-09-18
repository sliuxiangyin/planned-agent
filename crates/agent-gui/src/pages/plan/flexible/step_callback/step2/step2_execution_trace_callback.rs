//! `flexible_step2` 的**轨迹提取回调**：把真实工具执行轨迹登记为 `execution_trace` 产物。
//!
//! # 为什么单独一个回调，而不是塞进 step2 的定稿登记
//!
//! `execution_trace` 与其它产物的**来源根本不同**：它不来自子 agent 的输出 JSON（模型自述），
//! 而来自**会话历史**（系统记录）。把它做成链上独立一环，好处是：
//! - 定稿登记回调（`Step2Callback`，见同目录 `step2_callback.rs`）保持「只从输出 JSON 取产物」的单一职责；
//! - 轨迹导出/清洗的策略（[`export_cleaned_trace`]）与 prompt 无关，可单独测试；
//! - 将来别的 step 要写轨迹类产物，照抄这一环即可。
//!
//! # 链上位置
//!
//! 它挂在 step2 结果链的**最前面**（`prelude` → 本回调 → 定稿登记）。顺序理由：
//! 轨迹落库失败要 `Abort`，而那时**状态还没推进**（`current_step` 仍停在 step1 阶段），
//! 协调器重跑 step2 即可自愈；反过来先推进再落轨迹，就会留下「已 `executed` 却没有轨迹」
//! 的状态 —— step4/step5 会在缺轨迹的情况下继续，更难发现。
//!
//! 至于「没定稿却落了轨迹」这种脏状态，由前置分析挡住：非定稿它直接终止链，本回调根本不会执行。
//!
//! 本环不改对外结果（那是前置分析的活）：链上还有下一环就 `Next` 交下去，链尾则 `Accept`。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{ResultDecision, SubAgentCall, SubAgentResultCallback};
use serde_json::Map;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::super::analysis::require_analysis;
use super::super::commit::hand_off;
use super::step2_callback::AGENT;
use super::tool_trace::export_cleaned_trace;

/// 轨迹产物的 key（`flexible_state.products` 里的名字）。
const EXECUTION_TRACE_KEY: &str = "execution_trace";

/// 轨迹提取回调：从会话历史导出真实工具轨迹并落库。
pub(super) struct Step2ExecutionTraceCallback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态）。
    service: Arc<PlansFlexibleService>,
}

impl Step2ExecutionTraceCallback {
    pub(super) fn new(plan_id: String, service: Arc<PlansFlexibleService>) -> Self {
        Self { plan_id, service }
    }
}

#[async_trait]
impl SubAgentResultCallback for Step2ExecutionTraceCallback {
    async fn on_result(&self, call: &SubAgentCall<'_>) -> ResultDecision {
        // 归属：直接取前置分析的结论（它已校验过 `host_session_id` 的存在），
        // 不重复读 `ctx.arguments` —— 同一件事只在一处把关。
        let analysis = match require_analysis(AGENT, call) {
            Ok(analysis) => analysis,
            Err(decision) => return decision,
        };

        // 数据源是**会话历史**：模型自述的轨迹不可信，历史里真实发生过什么才算数。
        let trace = export_cleaned_trace(call.history);
        let count = trace.as_array().map_or(0, Vec::len);
        if count == 0 {
            // 空轨迹不是错误，恰恰是**要如实记录的事实**：本次一个业务工具都没调用。
            // 落库为空数组，下游（模板生成）一眼可见「这轮没有可复用的执行步骤」。
            tracing::warn!("[flexible_step2] 本次执行没有任何真实工具调用，execution_trace 为空");
        }

        let mut patch = Map::new();
        patch.insert(EXECUTION_TRACE_KEY.to_string(), trace);

        match self
            .service
            .merge_state(&self.plan_id, analysis.session_id, None, &patch)
            .await
        {
            Ok((step, _)) => tracing::info!(
                "[flexible_step2] execution_trace 已登记: plan_id={}, host_session_id={}, current_step={}, 调用数={}",
                self.plan_id,
                analysis.session_id,
                step,
                count,
            ),
            Err(e) => {
                // 与定稿登记同属**不可重试**的硬错误：重试也修不好，静默 Accept 会让协调器
                // 以为轨迹已就位（step4/step5 会在缺轨迹的情况下继续）。中断并如实上报。
                // 此刻状态尚未推进，协调器重跑 step2 即可自愈。
                let reason = format!(
                    "[flexible_step2] execution_trace 登记失败（plan_id={}, host_session_id={}）：{}",
                    self.plan_id, analysis.session_id, e
                );
                tracing::error!("{}", reason);
                return ResultDecision::Abort(reason);
            }
        }

        // 本环不改对外结果（前置分析已定稿）：这是链的中间环，把值交给下一环（定稿登记）。
        hand_off(call)
    }

    fn name(&self) -> &str {
        "flexible_step2_execution_trace"
    }
}
