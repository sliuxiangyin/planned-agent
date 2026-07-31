//! AI 客户端管理器 GUI 适配层

use std::sync::Arc;

use planned_agent_ai_manager::AiManager;
use planned_agent_core::types::AiProviderConfig;

use crate::config::AiProviderConfig as GuiAiProviderConfig;

/// GUI 层 AI 上下文
///
/// 组件通过 `use_context::<Resource<Option<Arc<AiContext>>>>()` 获取，
/// 再通过 `ctx.manager.default()` 或 `ctx.manager.get(name)` 拿到具体客户端。
pub struct AiContext {
    pub manager: Arc<AiManager>,
}

impl AiContext {
    /// 从 GUI 配置的 AI provider 列表同步初始化
    pub fn init(configs: &[GuiAiProviderConfig]) -> anyhow::Result<Self> {
        // 映射 GUI 配置 → core 配置（结构同型，但类型不同，便于将来 GUI 扩展）
        let core_configs: Vec<AiProviderConfig> = configs
            .iter()
            .map(|c| AiProviderConfig {
                name: c.name.clone(),
                provider: c.provider.clone(),
                api_key: c.api_key.clone(),
                model: c.model.clone(),
                max_tokens: c.max_tokens,
                temperature: c.temperature,
                base_url: c.base_url.clone(),
                is_default: c.is_default,
                thinking_config: c.thinking_config.clone(),
            })
            .collect();

        let manager = AiManager::from_config(core_configs)?;
        tracing::info!(
            "AI 管理器初始化完成: {} providers, default = {}",
            manager.provider_count(),
            if manager.has_default() { "yes" } else { "no" }
        );
        Ok(Self {
            manager: Arc::new(manager),
        })
    }
}