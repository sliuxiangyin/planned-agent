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
pub mod init_status;
pub mod kv;
pub mod mcp;
pub mod prompt;
pub mod rag;
pub mod storage;
pub mod tools;

pub use ai::AiContext;
pub use init_status::{InitStatus, ModuleState, ModuleStatus};
pub use kv::KvContext;
pub use mcp::{McpChangeNotifier, McpContext};
pub use prompt::PromptContext;
pub use rag::RagContext;
pub use storage::{StorageContext, storage_repo};
pub use tools::ToolsContext;

/// 从 Dioxus Context 取出已初始化的 `Resource<Option<Arc<T>>>` 并解包。
///
/// App 启动时通过 `use_resource` + `use_context_provider` 初始化，
/// 路由到子组件时必定 Ready。若调用过早（尚未初始化）则 panic。
pub fn require_resource<T: 'static>() -> Arc<T> {
    let resource = use_context::<Resource<Option<Arc<T>>>>();
    let guard = resource.read();
    guard
        .as_ref()
        .and_then(|x| x.as_ref())
        .cloned()
        .expect("Context Resource 尚未初始化——请确保 App 组件已注入该 Resource")
}