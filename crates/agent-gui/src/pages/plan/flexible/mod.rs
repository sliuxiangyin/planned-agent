//! 灵活模式组件模块：基于 v2_chat 的聊天面板（含子 agent 支持）。
//!
//! - `chat_flow` — 消息流转逻辑（发送 / 事件消费 / UI 交互回传）
//! - `page` — `FlexiblePage` 组件（消息列表 + 输入区 + 提示词选择器）

pub(crate) mod chat_flow;
pub(crate) mod page;

pub(crate) use page::FlexiblePage;
