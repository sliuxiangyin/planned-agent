//! `flexible_revise` 的组装点：结果链（前置分析 + 定稿登记两环）+ 启动前注入。
//!
//! 实现在同目录 `revise_callback.rs`（[`ReviseCallback`]，含本 step 的常量、定位替换纯函数与单测）；
//! 解析、守门与对外定稿在 [`super::prelude`]；零策略工具在 [`super::commit`] 与 [`super::analysis`]。
//!
//! 入参注入见 [`INJECT_MAPPING`]：**现状全量产物**由系统按 `host_session_id` 从 `flexible_state`
//! 直取注入，**不由协调器 LLM 转抄**。协调器只保留「进度权」（决定走哪个入口、按 `status` 路由）。
//!
//! 注：注入侧一律加 `current_` 前缀 —— 输入的 `steps` 是**现状全量**，而输出的 `steps` 是
//! **补丁数组**，同名会让 LLM 把两者互抄。改名机制与 `clarify` 把 `task_definition` 注入成
//! `previous_task_definition` 的做法一致。
//!
//! 设计见 `docs/planned-agent/flexible-incremental-revise.md`。

mod revise_callback;

use std::sync::Arc;

use planned_agent::chat::{SubAgentBeforeCallback, SubAgentResultChain};

use crate::pages::plan::shared::session::TemplateNotifier;
use crate::services::plans_flexible_service::PlansFlexibleService;

use super::before_inject::StateInjectCallback;
use super::prelude::FlexibleStepPrelude;
use revise_callback::{ReviseCallback, AGENT, OK_STATUS};

/// 创建 `flexible_revise` 结果链（方便传给 `register_sub_agent`）。
///
/// 链上两环：**前置分析**（解析 + 守门 + 定稿对外文本）+ 本 step 的定稿登记回调。
///
/// 注意：非 `revised` 的输出（`needs_restart` / `error`）由前置分析拦下 —— 链不跑，
/// 一个产物都不会写，协调器按 `status` 自己路由。
///
/// `notifier` 用于**已保存且用户授权更新时**（`current_step == saved` 且 `save_template`
/// 为 `true`）把修订落库后通知左侧面板重读模板 —— 否则改动只进 `flexible_state`，
/// `plans_flexible_sessions` 会停在旧值。
pub fn create_revise_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
    notifier: TemplateNotifier,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![Arc::new(ReviseCallback::new(
        plan_id, service, notifier,
    ))])
    .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}

/// 启动前注入映射：`(state 产物字段名, 注入到子 agent 参数的字段名)`。
///
/// 四个产物**全部注入且全部改名**（`current_` 前缀）：修订必须看到完整的现状才能判断
/// 「这次改动牵不牵连别处」。缺失的字段由 [`StateInjectCallback`] 跳过（不写 `null`），
/// 与「上游还没产出该产物」的语义一致。
///
/// 另外还注入档位 [`INJECTED_STEP_FIELD`]（走 [`StateInjectCallback::with_step_field`]）——
/// 它不在 `products` 里，故不在本映射内。
pub(super) const INJECT_MAPPING: &[(&str, &str)] = &[
    ("task_definition", "current_task_definition"),
    ("steps", "current_steps"),
    ("inputs", "current_inputs"),
    ("output_schema", "current_output_schema"),
];

/// 额外注入的档位字段名（`current_step` 是 state 的**列**，不在 `products` 里）。
///
/// revise 靠它判断「该会话是否已保存」—— 只有已保存才需要先问用户「要不要更新模板」。
const INJECTED_STEP_FIELD: &str = "current_step";

/// 创建 `flexible_revise` 的启动前注入回调（`StateInjectCallback` 的薄包装：
/// 映射留在本 step，改映射不必翻注册点）。
pub fn create_revise_inject(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Arc<dyn SubAgentBeforeCallback> {
    Arc::new(
        StateInjectCallback::new(plan_id, service, INJECT_MAPPING.to_vec())
            .with_step_field(INJECTED_STEP_FIELD),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 映射就是本 step 的入参契约：写错会静默传错数据，故与其它 step 一样锁住取值。
    /// 特别锁 `current_` 前缀 —— 去掉前缀会让输入的「现状全量 steps」与输出的「补丁 steps」同名。
    #[test]
    fn inject_mapping_matches_revise_input_contract() {
        assert_eq!(
            INJECT_MAPPING,
            &[
                ("task_definition", "current_task_definition"),
                ("steps", "current_steps"),
                ("inputs", "current_inputs"),
                ("output_schema", "current_output_schema"),
            ]
        );
        assert!(
            INJECT_MAPPING.iter().all(|(_, dst)| dst.starts_with("current_")),
            "注入侧必须统一带 current_ 前缀，避免与输出侧同名字段互抄"
        );
    }

    /// 档位注入字段名是 revise 与回调之间的契约（回调靠 `saved` 判断要不要落库）。
    #[test]
    fn injected_step_field_is_locked() {
        assert_eq!(INJECTED_STEP_FIELD, "current_step");
        assert!(
            !INJECT_MAPPING.iter().any(|(_, dst)| *dst == INJECTED_STEP_FIELD),
            "档位不走 mapping（它不是 products 字段），两处都写会重复注入"
        );
    }
}
