//! `flexible_save` 的组装点：结果链（前置分析 + 定稿登记）+ 启动前注入。
//!
//! 实现在同目录 `save_callback.rs`（[`SaveCallback`]，含本 step 的常量与单测）；
//! 解析、守门与对外定稿在 [`super::prelude`]；零策略工具在 [`super::commit`] 与 [`super::analysis`]。

mod save_callback;

use std::sync::Arc;

use planned_agent::chat::{SubAgentBeforeCallback, SubAgentResultChain};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::before_inject::StateInjectCallback;
use super::prelude::FlexibleStepPrelude;
use save_callback::{SaveCallback, AGENT, OK_STATUS};

/// 创建 `flexible_save` 结果链（方便传给 `register_sub_agent`）。
///
/// 链上两环：**前置分析**（解析 + 守门 + 定稿对外文本）+ 定稿登记回调（落库 + 推进 saved）。
pub fn create_save_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![Arc::new(SaveCallback::new(plan_id, service))])
        .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}

/// 启动前注入映射：`(state 产物字段名, 注入到子 agent 参数的字段名)`。
///
/// - `parameterized_task` ← `flexible_state.products.parameterized_task`（step2 定稿产物）。
pub(super) const INJECT_MAPPING: &[(&str, &str)] = &[
    ("parameterized_task", "parameterized_task"),
];

/// 创建 `flexible_save` 的启动前注入回调（映射留在本 step）。
pub fn create_save_inject(
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

    /// 映射就是本 step 的入参契约：写错会静默传错数据，故锁住取值。
    #[test]
    fn inject_mapping_matches_save_input_contract() {
        assert_eq!(INJECT_MAPPING, &[("parameterized_task", "parameterized_task")]);
    }
}
