//! Plan 模块的内部类型、纯数据模型与 `Message` 辅助函数。
//!
//! 仅 `plan` 子模块内部使用；状态容器（`ChatState` / `PlanState` / `WorkflowState`）
//! 已拆到同级 `states` 模块，本模块只保留不持有 `Signal` 的类型与辅助函数。
//!
//! 内部按领域拆分为四个子文件：
//! - `message` — `core::Message` 的显示辅助函数
//! - `pending_ui` — 待处理的 UI 交互状态（`PendingUIState`）
//! - `plan` — 计划元数据（`ParamDef` / `PlanSource` / `PlanGeneratedEvent` / `PlanInfo`）
//! - `workflow` — 灵活模式工作流类型（`WorkflowPhase` / `PlanFlexibleSnapshot` / `ExecutionStep` / `StepStatus`）
//!
//! 各子文件内项声明为 `pub(crate)`（re-export 提级到 `plan` 可见的必要条件，
//! 且 `plan` 模块本身是私有 `mod`，crate 外不可达，封装不受影响），
//! 本文件统一以 `pub(super)`（对 `plan` 可见）重新导出，对外可见性与原单文件
//! `types.rs` 保持一致，调用方（`states` / `chat` / `flexible` / `left_panel` /
//! `components` / `shared`）无需改动。

mod message;
mod pending_ui;
mod plan;
mod workflow;

pub(super) use message::{display_text, display_text_mut, role_css_class};
pub(super) use pending_ui::PendingUIState;
pub(super) use plan::{ParamDef, PlanGeneratedEvent, PlanInfo, PlanSource};
pub(super) use workflow::{ExecutionStep, PlanFlexibleSnapshot, StepStatus, WorkflowPhase};
