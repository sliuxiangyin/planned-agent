//! `flexible_output` 结果链的组装点：前置分析 → 定稿登记。
//!
//! # 链上两环
//!
//! 1. **默认前置分析**（[`FlexibleStepPrelude`]）：解析输出、定位会话、判定定稿，并定稿对外文本；
//!    解析失败 → 要求重新输出，非定稿 → 直接终止链（定稿登记不执行）。
//! 2. **定稿登记**（[`OutputCallback`]）：写 `output_schema`、推进 `output_defined`。
//!
//! 实现在同目录 `output_callback.rs`（定稿登记）；解析、守门与对外定稿在 [`super::prelude`]。
//!
//! 入参注入见 [`INJECT_MAPPING`]：任务定义 + 参数化产物由系统按 `host_session_id`
//! 从 `flexible_state` 直取注入，**不由协调器 LLM 转抄**。
//!
//! 设计见 `docs/planned-agent/flexible-output-step.md`。

mod output_callback;

use std::sync::Arc;

use planned_agent::chat::{SubAgentBeforeCallback, SubAgentResultChain};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::before_inject::StateInjectCallback;
use super::prelude::FlexibleStepPrelude;
use output_callback::{OutputCallback, AGENT, OK_STATUS};

/// 创建 `flexible_output` 结果链（方便传给 `register_sub_agent`）。
///
/// 链上两环：**前置分析**（解析 + 守门 + 定稿对外文本）+ 本 step 的定稿登记回调。
pub fn create_output_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![
        // 定稿登记：写 output_schema、推进 output_defined
        Arc::new(OutputCallback::new(plan_id, service)),
    ])
    // 前置分析：解析 + 守门 + 定稿对外文本
    .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}

/// 启动前注入映射：`(state 产物字段名, 注入到子 agent 参数的字段名)`。
///
/// - `task_definition` ← 需求澄清定稿产物（判断「这次任务本来要交付什么」）。
/// - `steps` ← 计划 / 参数化定稿的步骤骨架（看 `expected_output` 推断交付物）。
/// - `inputs` ← 参数化产出的参数表。
///
/// 后期「把执行结果与过程喂给输出步」（见设计稿 §2.7）只需在这里加一行映射，
/// 例如 `("execution_trace", "execution_trace")` —— 机制已由 [`StateInjectCallback`] 提供。
pub(super) const INJECT_MAPPING: &[(&str, &str)] = &[
    ("task_definition", "task_definition"),
    ("steps", "steps"),
    ("inputs", "inputs"),
];

/// 创建 `flexible_output` 的启动前注入回调（映射留在本 step）。
pub fn create_output_inject(
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
    fn inject_mapping_matches_output_input_contract() {
        assert_eq!(
            INJECT_MAPPING,
            &[
                ("task_definition", "task_definition"),
                ("steps", "steps"),
                ("inputs", "inputs"),
            ]
        );
    }
}
