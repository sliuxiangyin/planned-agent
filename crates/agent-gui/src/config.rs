use anyhow::Result;
use config::Config;
use serde::{Deserialize, Serialize};

/// agent-gui 完整配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    /// AI 提供商列表
    #[serde(default)]
    pub ai_providers: Vec<AiProviderConfig>,

    /// MCP 服务器列表
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,

    /// 日志配置
    #[serde(default)]
    pub logging: LoggingConfig,

    /// GUI 专属配置
    #[serde(default)]
    pub gui: GuiSettings,

    /// RAG 向量检索配置
    #[serde(default)]
    pub rag: RagConfig,
}

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
}

/// MCP 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub server_command: String,
    #[serde(default)]
    pub server_args: Vec<String>,
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub categories: Option<Vec<String>>,
}

fn default_transport() -> String {
    "stdio".to_string()
}

/// 日志配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_format")]
    pub format: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "pretty".to_string()
}

/// GUI 专属设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiSettings {
    /// 窗口标题
    #[serde(default = "default_window_title")]
    pub window_title: String,

    /// 窗口宽度
    #[serde(default = "default_window_width")]
    pub window_width: u32,

    /// 窗口高度
    #[serde(default = "default_window_height")]
    pub window_height: u32,

    /// 主题: "dark" | "light"
    #[serde(default = "default_theme")]
    pub theme: String,

    /// 默认视图: "chat" | "plan" | "trace"
    #[serde(default = "default_view")]
    pub default_view: String,
}

fn default_window_title() -> String {
    "Planned Agent".to_string()
}
fn default_window_width() -> u32 {
    1200
}
fn default_window_height() -> u32 {
    800
}
fn default_theme() -> String {
    "dark".to_string()
}
fn default_view() -> String {
    "chat".to_string()
}

// ── Default impls ──

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            ai_providers: Vec::new(),
            mcp_servers: Vec::new(),
            logging: LoggingConfig::default(),
            gui: GuiSettings::default(),
            rag: RagConfig::default(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
        }
    }
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            window_title: default_window_title(),
            window_width: default_window_width(),
            window_height: default_window_height(),
            theme: default_theme(),
            default_view: default_view(),
        }
    }
}

// ── RAG 配置 ──

/// RAG 向量检索完整配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagConfig {
    /// Embedding 提供商: "openai"
    #[serde(default = "default_embedding_provider")]
    pub embedding_provider: String,

    /// Embedding 模型名称
    #[serde(default)]
    pub embedding_model: String,

    /// Embedding API 基础 URL
    #[serde(default)]
    pub embedding_base_url: String,

    /// Embedding API Key
    #[serde(default)]
    pub embedding_api_key: String,

    /// 向量存储配置
    #[serde(default)]
    pub store: RagStoreConfig,

    /// 检索配置
    #[serde(default)]
    pub retrieval: RagRetrievalConfig,
}

fn default_embedding_provider() -> String {
    "openai".to_string()
}

/// 向量存储配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagStoreConfig {
    /// 向量存储路径
    #[serde(default = "default_rag_store_path")]
    pub path: String,
}

fn default_rag_store_path() -> String {
    "./traces/vector_store".to_string()
}

/// 检索参数配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagRetrievalConfig {
    /// 默认返回数量
    #[serde(default = "default_top_k")]
    pub top_k: usize,

    /// 相似度门槛 (0~1)
    #[serde(default = "default_similarity_threshold")]
    pub similarity_threshold: f32,
}

fn default_top_k() -> usize {
    5
}

fn default_similarity_threshold() -> f32 {
    0.7
}

// ── Default impls ──

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            embedding_provider: default_embedding_provider(),
            embedding_model: String::new(),
            embedding_base_url: String::new(),
            embedding_api_key: String::new(),
            store: RagStoreConfig::default(),
            retrieval: RagRetrievalConfig::default(),
        }
    }
}

impl Default for RagStoreConfig {
    fn default() -> Self {
        Self {
            path: default_rag_store_path(),
        }
    }
}

impl Default for RagRetrievalConfig {
    fn default() -> Self {
        Self {
            top_k: default_top_k(),
            similarity_threshold: default_similarity_threshold(),
        }
    }
}

// ── 加载逻辑 ──

impl GuiConfig {
    /// 从 `config.toml` 加载配置，失败时返回默认配置
    pub fn load() -> Self {
        match Self::try_load() {
            Ok(cfg) => {
                tracing::info!("配置加载成功: config.toml");
                cfg
            }
            Err(e) => {
                tracing::warn!("配置加载失败，使用默认配置: {}", e);
                Self::default()
            }
        }
    }

    fn try_load() -> Result<Self> {
        let cfg = Config::builder()
            .add_source(config::File::with_name("config"))
            .build()?;

        let gui_config: GuiConfig = cfg.try_deserialize()?;
        Ok(gui_config)
    }

    /// 获取默认 AI 提供商
    pub fn default_ai_provider(&self) -> Option<&AiProviderConfig> {
        self.ai_providers
            .iter()
            .find(|p| p.is_default)
            .or_else(|| self.ai_providers.first())
    }
}
