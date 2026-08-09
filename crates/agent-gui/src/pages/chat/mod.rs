//! Chat 测试页模块：独立的聊天调试界面。
//!
//! 用途：调试 `request_user_action` 交互展示（confirm / select / input / multi_select）。
//! - `page` — `ChatPage` 组件（顶栏 + 消息列表 + 输入区 + 提示词选择器）
//! - `chat_flow` — 聊天消息流转逻辑（发送 / 流式消费 / 用户操作回传，纯内存不持久化）

pub(crate) mod chat_flow;
pub(crate) mod page;

pub(crate) use page::ChatPage;
