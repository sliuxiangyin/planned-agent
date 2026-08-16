//! 灵活模式组件模块：基于 v2_chat 的聊天面板（含子 agent 支持）。
//!
//! - `page` — `FlexiblePage` 组件（子 agent 注册 + ChatPanel 集成）

pub(crate) mod page;

pub(crate) use page::FlexiblePage;
