use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Prompt管理器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptManagerConfig {
    /// prompt文件目录
    pub prompt_dir: PathBuf,
    /// 模板引擎配置
    pub template_engine: TemplateEngineConfig,
    /// 缓存配置
    pub cache: CacheConfig,
}

/// 模板引擎配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateEngineConfig {
    /// 是否自动重新加载模板
    pub auto_reload: bool,
}

/// 缓存配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// 是否启用缓存
    pub enabled: bool,
    /// 最大缓存条目数
    pub max_size: usize,
    /// 缓存过期时间（秒）
    pub ttl_seconds: u64,
}

impl Default for PromptManagerConfig {
    fn default() -> Self {
        Self {
            prompt_dir: PathBuf::from("./prompts"),
            template_engine: TemplateEngineConfig {
                auto_reload: true,
            },
            cache: CacheConfig {
                enabled: true,
                max_size: 1000,
                ttl_seconds: 3600,
            },
        }
    }
}

impl PromptManagerConfig {
    /// 从配置文件加载
    pub fn from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }
}
