//! `flexible_step2` 结果链的组装点：前置分析 → 轨迹提取 → 定稿登记（顺序不可颠倒）。
//!
//! # 链上三环
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
//! 实现在同目录：`step2_callback.rs`（定稿登记）与 `step2_execution_trace_callback.rs`
//! （轨迹提取，导出 / 清洗策略在 `tool_trace.rs`）；解析、守门与对外定稿在 [`super::prelude`]。

mod step2_callback;
mod step2_execution_trace_callback;
mod tool_trace;

use std::sync::Arc;

use planned_agent::chat::SubAgentResultChain;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::prelude::FlexibleStepPrelude;
use step2_execution_trace_callback::Step2ExecutionTraceCallback;
use step2_callback::{Step2Callback, AGENT, OK_STATUS};

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
        Arc::new(Step2Callback::new(plan_id, service)),
    ])
    // ① 前置分析：解析 + 守门 + 定稿对外文本
    .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}
