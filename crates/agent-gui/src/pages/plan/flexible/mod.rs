//! 灵活模式组件模块：基于 chat 的聊天面板（含子 agent 支持）。
//!
//! - `page` — `FlexiblePage` 组件（壳：会话集合 + plan 级注册/模板）
//! - `session_host` — `FlexibleSessionHost` 组件（单会话常驻宿主）
//! - `tool` — 协调器旁路工具（`flexible_state` / `flexible_save_template`）

pub(crate) mod chat_flexible_message_storage;
pub(crate) mod chat_service_factory;
pub(crate) mod controller;
pub(crate) mod page;
pub(crate) mod session_boot;
pub(crate) mod session_host;
pub(crate) mod step2_callback;
pub(crate) mod tool;

pub(crate) use page::FlexiblePage;
