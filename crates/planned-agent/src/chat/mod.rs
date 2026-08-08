//! Chat 模块 —— 多轮对话 + 工具调用服务
//!
//! 提供 [`ChatService`],可接收调用方维护的 `Vec<Message>` 历史,
//! 自动注入 system prompt,通过 AI 流式 API 发送请求,
//! 处理多轮 tool-call 循环,并以 [`ChatEvent`] 回调的形式实时下发增量结果。
//!
//! # 核心类型
//!
//! | 类型 | 文件 | 说明 |
//! |---|---|---|
//! | [`ChatConfig`] | [`config`] | 纯数据配置 |
//! | [`ChatService`] | [`service`] | 聊天服务入口 |
//! | [`ChatResponse`] | [`service`] | 完整响应(由 `chat_with_callback` 返回) |
//! | [`ChatEvent`] | [`event`] | 流式事件枚举(通过回调实时下发) |
//! | [`UIAction`] | [`ui_action`] | UI 交互动作(通过 `ChatEvent::UIActionRequest` 下发) |

pub mod config;
pub mod event;
pub mod service;
pub mod ui_action;

// 公开给外部使用的核心类型(符合 "只导出三个主类型" 的设计决策)
pub use config::ChatConfig;
pub use event::ChatEvent;
pub use service::{ChatResponse, ChatService, PendingUIAction};
pub use ui_action::{MultiSelectOption, UIAction, UIActionType};
