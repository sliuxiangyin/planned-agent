//! `flexible_step1` 结果链的组装点：前置分析 + 定稿登记两环。
//!
//! 实现在同目录 `step1_callback.rs`（[`Step1Callback`]，含本 step 的常量与单测）；
//! 解析、守门与对外定稿在 [`super::prelude`]；零策略工具在 [`super::commit`] 与 [`super::analysis`]。

mod step1_callback;

use std::sync::Arc;

use planned_agent::chat::SubAgentResultChain;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::prelude::FlexibleStepPrelude;
use step1_callback::{Step1Callback, AGENT, OK_STATUS};

/// 创建 `flexible_step1` 结果链（方便传给 `register_sub_agent`）。
///
/// 链上两环：**前置分析**（解析 + 守门 + 定稿对外文本）+ 本 step 的定稿登记回调。
pub fn create_step1_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![Arc::new(Step1Callback::new(plan_id, service))])
        .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}
