//! 设置页面模块
//!
//! 结构：
//! - `types` — SettingsTab、筛选枚举
//! - `page` — `SettingsPage` 组件（对外暴露）
//! - `components` — 子组件（tool_list、mcp_server_manager）

mod components;
mod page;
mod types;

pub use page::SettingsPage;
