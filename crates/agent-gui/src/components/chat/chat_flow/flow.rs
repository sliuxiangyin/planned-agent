//! 聊天消息流转逻辑（已移入 `signals.rs` 的 `ChatSignals` 方法中）。
//!
//! 保留此文件仅为模块结构清晰，所有逻辑在 `ChatSignals::send_message`、
//! `ChatSignals::ensure_subscription`、`ChatSignals::handle_event`、
//! `ChatSignals::handle_user_action` 中。
