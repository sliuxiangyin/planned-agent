//! Plan 页面左侧面板模块。
//!
//! 从 `page.rs` 抽离的左栏 UI：顶栏（返回 + 名称 + 模式/状态 chip + 更多操作）、
//! 四个 Bento 瓷块（PIPELINE / PARAMS / STATS / HISTORY）与「删除计划」确认弹窗。
//!
//! 内部结构：
//! - `left_panel` — `PlanLeftPanel` 容器组件（对外暴露）
//! - `pipeline` — PIPELINE 执行时间线块
//! - `params` — PARAMS 参数块
//! - `stats` — STATS 统计块
//! - `history` — HISTORY 历史版本块
//! - `dialogs` — 「删除计划」确认弹窗
//!
//! 样式沿用全局 `assets/plan-left-panel.css`（经 `asset!` 按需加载），
//! 各块当前为静态展示，数据接入留待后续任务。

mod dialogs;
mod history;
mod left_panel;
mod params;
mod pipeline;
mod stats;

pub use left_panel::PlanLeftPanel;
