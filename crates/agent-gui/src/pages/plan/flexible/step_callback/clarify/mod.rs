//! `flexible_clarify` 的组装点：结果链（前置分析 + 定稿登记两环）+ 启动前注入。
//!
//! 实现在同目录 `clarify_callback.rs`（[`ClarifyCallback`]，含本 step 的常量与单测）；
//! 解析、守门与对外定稿在 [`super::prelude`]；零策略工具在 [`super::commit`] 与 [`super::analysis`]。
//!
//! 入参注入见 [`INJECT_MAPPING`]：任务基线由系统按 `host_session_id` 从 `flexible_state` 直取
//! 注入，**不由协调器 LLM 转抄**。协调器只保留「进度权」（调哪个 step、按 `status` 路由）。

mod clarify_callback;

use std::sync::Arc;

use planned_agent::chat::{SubAgentBeforeCallback, SubAgentResultChain};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::before_inject::StateInjectCallback;
use super::prelude::FlexibleStepPrelude;
use clarify_callback::{ClarifyCallback, AGENT, OK_STATUS};

/// 创建 `flexible_clarify` 结果链（方便传给 `register_sub_agent`）。
///
/// 链上两环：**前置分析**（解析 + 守门 + 定稿对外文本）+ 本 step 的定稿登记回调。
pub fn create_clarify_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![Arc::new(ClarifyCallback::new(plan_id, service))])
        .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}

/// 启动前注入映射：`(state 产物字段名, 注入到子 agent 参数的字段名)`。
///
/// 需求基线的**唯一来源**：按 `host_session_id` 从 `flexible_state.products` 取
/// `task_definition`，注入为子 agent 侧看到的 `previous_task_definition`
/// （顶层合并、同名覆盖，见 [`StateInjectCallback`]）。
/// 首次澄清时 state 无该项 ⇒ 不注入 —— 与「首次不提供基线」的既有语义一致。
///
/// 重做需求澄清时 state 里仍是上一版基线 ⇒ 注入的也是它：基线的来源始终只有 state 一处，
/// 与 `flexible_clarify.toml` 的输入说明一致（不再依赖协调器 LLM 的对话记忆）。
pub(super) const INJECT_MAPPING: &[(&str, &str)] = &[
    ("task_definition", "previous_task_definition"),
];

/// 创建 `flexible_clarify` 的启动前注入回调（`StateInjectCallback` 的薄包装：
/// 映射留在本 step，改映射不必翻注册点）。
pub fn create_clarify_inject(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Arc<dyn SubAgentBeforeCallback> {
    Arc::new(StateInjectCallback::new(
        plan_id,
        service,
        INJECT_MAPPING.to_vec(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 映射就是本 step 的入参契约：写错会静默传错数据，故与 `stepN_callback` 的常量一样锁住取值。
    #[test]
    fn inject_mapping_matches_clarify_input_contract() {
        assert_eq!(
            INJECT_MAPPING,
            &[("task_definition", "previous_task_definition")]
        );
    }
}
