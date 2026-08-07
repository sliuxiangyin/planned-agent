//! Plan 页面共享辅助模块。
//!
//! - `load_plan_data` — 加载计划元数据、历史消息（周密）或 plans_flexible 快照（灵活）
//! - `save_flexible_plan` — 灵活模式计划保存（含 output_schema 推断与持久化）

pub mod load_plan_data;
pub mod save_flexible_plan;
