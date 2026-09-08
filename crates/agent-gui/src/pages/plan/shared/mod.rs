//! Plan 页面共享辅助模块。
//!
//! - `load_plan_data` — 加载计划元数据
//! - `session` — 会话状态管理中心（SessionManager，watch + dioxus 双通道）

pub mod load_plan_data;
pub mod session;