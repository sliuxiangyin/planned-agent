//! 对外 API 层：`ChatService` 及其配套的公开类型。
//!
//! - `service.rs`：[`ChatService`] —— 有状态、后台 loop、事件订阅的聊天服务入口；
//! - `ticket.rs`：[`SendTicket`] —— `send` 的完成凭证（可 `await`）；
//! - `config.rs`：[`ChatConfig`] —— 纯数据配置；
//! - `event.rs`：[`ChatEvent`] / [`SubscriptionId`] / [`SubscriptionGuard`] ——
//!   事件协议、订阅 ID、RAII 守卫。
//!
//! 本层只负责「对外接口 + 入队」，不含后台执行逻辑：
//! `send` / `confirm_user_action` 把命令写入队列后立即返回，
//! 真正的多轮对话循环在 [`crate::chat::driver`]。

mod config;
mod event;
mod service;
mod ticket;

pub use config::ChatConfig;
pub use event::{SubscriptionGuard, SubscriptionId, ChatEvent};
pub use service::ChatService;
pub use ticket::{SendOutcome, SendTicket};
