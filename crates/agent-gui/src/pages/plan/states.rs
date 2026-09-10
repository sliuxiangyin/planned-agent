//! Plan 模块的状态容器：Signal 状态结构体与方法。
//!
//! 与 `types`（纯类型/数据模型）分离，仅 `plan` 子模块内部使用，
//! 所有项以 `pub(super)` 暴露给同级模块。
//!
//! 待处理的 UI 交互状态使用 [`crate::components::chat::chat_flow::types::PendingUI`]；
//! 本模块只保留计划元数据状态 `PlanState`。
//!
//! 注：计划本身（`plan::Model` / mode）现由 `PlanPage` 直接读 `plan_resource` 得到，
//! 不再经此处 signal 中转，故 `plan_info` / `plan_mode` 及对应方法已移除。

use dioxus::prelude::*;

use super::types::ParamDef;

/// 计划元数据状态：版本号、已固化参数。
#[allow(dead_code)] // 预留：版本号 / 参数暂未接线
#[derive(Clone, Copy, PartialEq)]
pub(super) struct PlanState {
    pub plan_version: Signal<u32, SyncStorage>,
    /// 已固化的参数定义（清晰度检查 multi_select 勾选后暂存，确认生成时随事件落库）
    pub plan_params: Signal<Vec<ParamDef>, SyncStorage>,
}

// ── PlanState 方法 ──

impl PlanState {
    /// 覆盖暂存的固化参数（每次清晰度检查确认后以最新勾选为准）。
    #[allow(dead_code)] // 预留：待清晰度检查接线后调用
    pub fn set_params(&mut self, params: Vec<ParamDef>) {
        self.plan_params.set(params);
    }
}
