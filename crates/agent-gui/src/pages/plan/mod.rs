//! Plan 页面模块入口。
//!
//! 内部结构：
//! - `types` — 内部纯类型、数据模型与 `Message` 辅助函数
//! - `states` — Signal 状态容器（`ChatState` / `PlanState` / `WorkflowState`）与方法
//! - `page` — `PlanPage` 组件（对外暴露）
//! - `components` — 共享 UI 组件
//! - `shared` — 共享辅助模块（加载/保存）
//! - `flexible` — 灵活模式工作流组件
//! - `left_panel` — 左侧面板（顶栏 + Bento 瓷块 + 删除弹窗）
//!
//! 仅 `PlanPage` 通过 `pub use` 对外暴露，保持内部 API 收敛。

pub(crate) mod components;
mod flexible;
mod left_panel;
mod page;
mod shared;
mod states;
mod types;

pub use page::PlanPage;
