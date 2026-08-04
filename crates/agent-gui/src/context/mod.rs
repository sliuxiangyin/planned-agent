//! 服务层集成适配层（GUI 与核心模块的桥梁）
//!
//! 5 个 Context 模块共同把 `ai-manager` / `mcp-rmcp` / `prompt-manager` /
//! `tool-manager` / `rag` 的异步/同步组件聚合成 Dioxus 友好的 `Arc<...>`，
//! 通过 `Resource` 注入到 UI 组件树。
//!
//! 装配顺序与失败容忍策略见 `.qoder/plans/agent-gui-service-init.md`。

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
pub use storage::StorageContext;
pub use tools::ToolsContext;