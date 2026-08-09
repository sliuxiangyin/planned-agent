use serde::{Deserialize, Serialize};

/// 思考模式配置（适用于支持思考模式的AI模型）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// 思考模式开关：enabled/disabled
    pub enabled: bool,
    /// 思考强度：high/max（默认high，复杂Agent类请求自动设置为max）
    pub effort: Option<String>,
}

/// AI 提供商配置（支持多个）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiProviderConfig {
    pub name: String,
    pub provider: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub base_url: Option<String>,
    pub is_default: bool,
    /// 思考模式配置（适用于支持思考模式的AI模型）
    pub thinking_config: Option<ThinkingConfig>,
}
