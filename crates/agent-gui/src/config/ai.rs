//! AI 提供商配置（`[[ai_providers]]`）。

use planned_agent_core::ai::config::ThinkingConfig;
use serde::{Deserialize, Serialize};

/// AI 提供商配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiProviderConfig {
    pub name: String,
    pub provider: String,
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub is_default: bool,
    /// 思考模式配置（适用于支持思考模式的 AI 模型）
    #[serde(default)]
    pub thinking_config: Option<ThinkingConfig>,
}
