//! 灵活模式组件模块：基于 chat 的聊天面板（含子 agent 支持）。
//!
//! - `page` — `FlexiblePage` 组件（子 agent 注册 + ChatPanel 集成）

pub(crate) mod chat_flexible_message_storage;
pub(crate) mod controller;
pub(crate) mod json_extract;
pub(crate) mod page;
pub(crate) mod step2_callback;
pub(crate) mod step5_callback;

pub(crate) use page::FlexiblePage;
