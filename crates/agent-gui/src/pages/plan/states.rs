//! Plan 模块的状态容器：Signal 状态结构体与方法。
//!
//! 与 `types`（纯类型/数据模型）分离，仅 `plan` 子模块内部使用，
//! 所有项以 `pub(super)` 暴露给同级模块。
//!
//! 待处理的 UI 交互状态使用 [`crate::components::chat::chat_flow::types::PendingUI`]；
//! 本模块只保留计划元数据状态 `PlanState`。

use dioxus::prelude::*;

use super::types::{ParamDef, PlanInfo};

/// 计划元数据状态：模式、版本号、基本信息。
#[derive(Clone, Copy, PartialEq)]
pub(super) struct PlanState {
    pub plan_info: Signal<Option<PlanInfo>, SyncStorage>,
    pub plan_mode: Signal<Option<String>, SyncStorage>,
    pub plan_version: Signal<u32, SyncStorage>,
    /// 已固化的参数定义（清晰度检查 multi_select 勾选后暂存，确认生成时随事件落库）
    pub plan_params: Signal<Vec<ParamDef>, SyncStorage>,
}

// ── PlanState 方法 ──

impl PlanState {
    /// 获取当前计划模式字符串（默认为空串）。
    pub fn mode(&self) -> String {
        self.plan_mode.read().clone().unwrap_or_default()
    }

    /// 设置计划模式。
    pub fn set_mode(&mut self, mode: String) {
        self.plan_mode.set(Some(mode));
    }

    /// 覆盖暂存的固化参数（每次清晰度检查确认后以最新勾选为准）。
    pub fn set_params(&mut self, params: Vec<ParamDef>) {
        self.plan_params.set(params);
    }
}
