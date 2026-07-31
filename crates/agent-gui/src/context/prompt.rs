//! Prompt 管理器 GUI 适配层

use std::sync::Arc;

use planned_agent_core::prompt::PromptManager;
use planned_agent_prompt_manager::{FilePromptManager, PromptManagerConfig};

/// GUI 层 Prompt 上下文
///
/// 组件通过 `use_context::<Resource<Option<Arc<PromptContext>>>>()` 获取，
/// 再通过 `ctx.manager.render(name, &ctx)` 等 API 渲染/解析 prompt。
pub struct PromptContext {
    pub manager: Arc<FilePromptManager>,
}

impl PromptContext {
    /// 从 PromptManagerConfig 异步初始化（加载 `prompt_dir` 下所有 prompt）
    pub async fn init(config: &PromptManagerConfig) -> anyhow::Result<Self> {
        let manager = FilePromptManager::new(config.clone())?;
        manager.initialize().await?;
        tracing::info!(
            "Prompt 管理器初始化完成: {} prompts (dir = {:?})",
            manager.list_prompts().await.map(|v| v.len()).unwrap_or(0),
            config.prompt_dir
        );
        Ok(Self {
            manager: Arc::new(manager),
        })
    }
}