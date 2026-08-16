//! v2 聊天模块 —— 有状态、后台 loop、事件订阅的多轮对话服务（重构版）
//!
//! 相对 v1 [`crate::chat`] 的改进：
//!
//! - **内部管理消息历史**：调用方不再每次传入 `Vec<Message>`；
//! - **`send` 不堵塞**：发送单条消息即返回，后台 driver task 异步执行
//!   完整多轮 tool-call 循环；
//! - **事件订阅**：[`ChatService::on_chat`] 注册监听（多订阅者、可取消），
//!   事件类型 [`ChatEvent`] 复用 core [`ChatEvent`] 并补充
//!   `Done` / `Error` 生命周期信号；
//! - **UI 交互按 tool 消息回填**：[`ChatService::confirm_user_action`]
//!   把用户选择压入与 `tool_call_id` 对应的 tool 消息，保持
//!   assistant(tool_calls) → tool 消息的协议顺序，loop 不重启、可持续多轮确认；
//! - **暂不含子 agent**（对应 v1 的 `SubAgentChatEvent` / `resume_sub_agent`）。
//!
//! # 核心类型
//!
//! | 类型 | 模块 | 说明 |
//! |---|---|---|
//! | [`ChatService`] | `service` | 有状态聊天服务入口 |
//! | [`SendTicket`] | `service` | `send` 的完成凭证（可 `await`） |
//! | [`ChatEvent`] | `service` | 事件协议（core `ChatEvent` + Done/Error） |
//! | [`ChatConfig`] | `service` | 纯数据配置 |
//!
//! # 目录结构
//!
//! ```text
//! chat/
//! ├── mod.rs      入口：模块声明 + 对外重导出（见下）
//! ├── service/    对外 API 层：ChatService / SendTicket / ChatConfig /
//! │               ChatEvent（只负责接口与入队，无后台逻辑）
//! ├── state/      内部状态层：State 容器 + History / Subscribers 封装、
//! │               Command / RunState 枚举、ToolCallAccumulator
//! ├── driver/     后台 driver：driver_loop（串行队列）+ run_conversation
//! │               （多轮 loop）+ await_confirm（UI 确认）+ inject_system_prompt
//! ├── tools/      工具：build_tool_definitions（白名单过滤）+
//! │               parse_ui_actions（request_user_action 参数解析）
//! └── tests.rs    集成测试（9 个用例）
//! ```
//!
//! 详细说明见本目录 `README.md`。

mod driver;
mod service;
mod state;
mod sub_agent;
mod tools;

#[cfg(test)]
mod tests;

pub use service::{ChatConfig, ChatEvent, ChatService, SendTicket, SubscriptionGuard, SubscriptionId};
pub use sub_agent::{ChatSubAgentSession, SubAgentRunner};
