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
//! | [`ChatEvent`] | [`event`] | 流式事件枚举(协议类型在 core 层,此处 re-export) |
//! | [`SubAgentChatEvent`] | [`event`] | 子 agent 过程流旁路事件(携带原始 `ChatEvent`) |
//! | [`UIAction`] | core `events` | UI 交互动作(通过 `ChatEvent::UIActionRequest` 下发) |

pub mod config;
pub mod event;
pub mod service;

// 公开给外部使用的核心类型(符合 "只导出三个主类型" 的设计决策)
pub use config::ChatConfig;
pub use event::{ChatEvent, SubAgentChatEvent};
pub use service::{ChatResponse, ChatService, PendingUIAction, SubAgentResumeOutcome};
// UIAction 系协议类型已下沉到 core::events，这里 re-export 保持路径不变
pub use planned_agent_core::events::{
    FALLBACK_CONFIRM_ID, FALLBACK_CONFIRM_LABEL, MultiSelectOption, UIAction, UIActionType,
};
