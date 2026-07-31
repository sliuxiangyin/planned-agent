//! 初始化状态聚合（仅数据层，UI 后续 PR 接入）

use dioxus::prelude::{ReadableExt, Resource};
use std::sync::Arc;

use super::{AiContext, McpContext, PromptContext, RagContext, ToolsContext};

/// 模块生命周期状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleState {
    /// Resource 尚未 settle（启动瞬间）
    Init,
    /// 初始化成功
    Ready,
    /// 初始化失败
    Failed,
}

/// 单个模块的初始化结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleStatus {
    pub state: ModuleState,
    /// 简短错误描述（静态字符串，避免持有 String）
    pub error: Option<&'static str>,
}

impl ModuleStatus {
    pub const fn ready() -> Self {
        Self {
            state: ModuleState::Ready,
            error: None,
        }
    }
    pub const fn failed(msg: &'static str) -> Self {
        Self {
            state: ModuleState::Failed,
            error: Some(msg),
        }
    }
    pub const fn init() -> Self {
        Self {
            state: ModuleState::Init,
            error: None,
        }
    }
}

/// 5 个模块的统一状态快照
///
/// 在 `app()` 中 5 个 Resource 都 settle 后构造一次，通过 `use_context_provider` 注入。
/// `tools` 的 Ready 仅表示内置工具就绪，**不**反映 MCP 注入状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitStatus {
    pub ai: ModuleStatus,
    pub prompt: ModuleStatus,
    pub mcp: ModuleStatus,
    pub tools: ModuleStatus,
    pub rag: ModuleStatus,
}

impl InitStatus {
    /// 从 5 个 Resource 构造状态快照
    pub fn from_resources(
        ai: &Resource<Option<Arc<AiContext>>>,
        prompt: &Resource<Option<Arc<PromptContext>>>,
        mcp: &Resource<Option<Arc<McpContext>>>,
        tools: &Resource<Option<Arc<ToolsContext>>>,
        rag: &Resource<Option<Arc<RagContext>>>,
    ) -> Self {
        fn map_status<T>(r: &Resource<Option<Arc<T>>>) -> ModuleStatus {
            match r.read().as_ref() {
                Some(Some(_)) => ModuleStatus::ready(),
                Some(None) => ModuleStatus::failed("init returned None"),
                None => ModuleStatus::init(),
            }
        }

        Self {
            ai: map_status(ai),
            prompt: map_status(prompt),
            mcp: map_status(mcp),
            tools: map_status(tools),
            rag: map_status(rag),
        }
    }

    /// 是否所有模块都已 settle（不论成败）
    pub fn all_settled(&self) -> bool {
        self.ai.state != ModuleState::Init
            && self.prompt.state != ModuleState::Init
            && self.mcp.state != ModuleState::Init
            && self.tools.state != ModuleState::Init
            && self.rag.state != ModuleState::Init
    }

    /// 是否所有 settle 的模块都 Ready
    pub fn all_ready(&self) -> bool {
        self.all_settled()
            && self.ai.state == ModuleState::Ready
            && self.prompt.state == ModuleState::Ready
            && self.mcp.state == ModuleState::Ready
            && self.tools.state == ModuleState::Ready
            && self.rag.state == ModuleState::Ready
    }
}