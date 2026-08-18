//! 通用聊天组件模块。
//!
//! - `chat_flow` — 消息流转逻辑（发送 / 事件消费 / UI 交互回传）
//! - `chat_panel` — 完整聊天面板（消息列表 + 输入区 + composer 工具栏）
//! - `chat_ui_actions_view` — Agent 交互卡片（Confirm / Select / Input / MultiSelect）
//! - `reasoning_view` — 深度思考折叠面板
//! - `tool_view` — Tool 调用详情折叠面板

pub mod chat_flow;
pub mod chat_panel;
pub mod chat_ui_actions_view;
pub mod reasoning_view;
pub mod tool_view;

pub use chat_panel::ChatPanel;
