//! 服务层集成适配层（GUI 与核心模块的桥梁）
//!
//! 5 个 Context 模块共同把 `ai-manager` / `mcp-rmcp` / `prompt-manager` /
//! `tool-manager` / `rag` 的异步/同步组件聚合成 Dioxus 友好的 `Arc<...>`，
//! 通过 `Resource` 注入到 UI 组件树。
//!
//! 装配顺序与失败容忍策略见 `.qoder/plans/agent-gui-service-init.md`。

use std::sync::Arc;
use dioxus::prelude::*;

pub mod ai;
pub mod boot;
pub mod kv;
pub mod mcp;
pub mod prompt;
pub mod rag;
pub mod storage;
pub mod sub_agent;
pub mod tools;

pub use ai::AiContext;
pub use boot::{BootPhase, ReadyServices, bootstrap};
pub use kv::KvContext;
pub use mcp::{McpChangeNotifier, McpContext};
pub use prompt::PromptContext;
pub use rag::RagContext;
pub use storage::StorageContext;
pub use sub_agent::register_sub_agent;
pub use tools::ToolsContext;

/// 从 Dioxus Context 取出已注入的 `Arc<T>`（就绪后恒存在）。
///
/// 启动门 [`bootstrap`] 全部成功后才渲染 `ReadyShell`，由它注入纯 `Arc<T>`；
/// 因此在子组件中调用必然能取到。若仍取不到（层级/时机错误）则 panic，
/// 帮助尽早暴露装配 bug。
pub fn require_resource<T: 'static>() -> Arc<T> {
    use_context::<Arc<T>>()
}