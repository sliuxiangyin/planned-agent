//! `flexible_step2` 结果链的组装点：前置分析 → 定稿登记。
//!
//! # 链上两环
//!
//! 1. **默认前置分析**（[`FlexibleStepPrelude`]）：解析输出、定位会话、判定定稿，并定稿对外文本；
//!    解析失败 → 要求重新输出，非定稿 → 直接终止链（定稿登记不执行）。
//! 2. **定稿登记**（[`Step2Callback`]）：写 `parameterized_task`、推进 `parameterized`。
//!
//! 实现在同目录 `step2_callback.rs`（定稿登记）；解析、守门与对外定稿在 [`super::prelude`]。
//!
//! 入参注入见 [`INJECT_MAPPING`]：任务定义由系统按 `host_session_id`
//! 从 `flexible_state` 直取注入，**不由协调器 LLM 转抄**。协调器只保留「进度权」。

mod step2_callback;

use std::sync::Arc;

use planned_agent::chat::{SubAgentBeforeCallback, SubAgentResultChain};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::before_inject::StateInjectCallback;
use super::prelude::FlexibleStepPrelude;
use step2_callback::{Step2Callback, AGENT, OK_STATUS};

/// 创建 `flexible_step2` 结果链（方便传给 `register_sub_agent`）。
///
/// 链上两环：**前置分析**（解析 + 守门 + 定稿对外文本）+ 本 step 的定稿登记回调。
pub fn create_step2_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![
        // 定稿登记：写 parameterized_task、推进 parameterized
        Arc::new(Step2Callback::new(plan_id, service)),
    ])
    // 前置分析：解析 + 守门 + 定稿对外文本
    .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}

/// 启动前注入映射：`(state 产物字段名, 注入到子 agent 参数的字段名)`。
///
/// - `task_definition` ← `flexible_state.products.task_definition`（step1 定稿产物）。
pub(super) const INJECT_MAPPING: &[(&str, &str)] = &[
    ("task_definition", "task_definition"),
];

/// 创建 `flexible_step2` 的启动前注入回调（映射留在本 step）。
pub fn create_step2_inject(
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
    fn inject_mapping_matches_step2_input_contract() {
        assert_eq!(
            INJECT_MAPPING,
            &[("task_definition", "task_definition")]
        );
    }
}
