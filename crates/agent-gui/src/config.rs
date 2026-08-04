use anyhow::Result;
use config::Config;
use planned_agent_core::types::ThinkingConfig;
use planned_agent_prompt_manager::PromptManagerConfig;
use serde::{Deserialize, Serialize};

/// agent-gui 完整配置
///
/// 注意：MCP 服务器配置已迁移到 `data/mcp-config.json`，由 `McpConfigService` 管理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    /// AI 提供商列表
    #[serde(default)]
    pub ai_providers: Vec<AiProviderConfig>,

    /// Prompt 管理器配置
    #[serde(default)]
    pub prompt_manager: PromptManagerConfig,

    /// 日志配置
    #[serde(default)]
    pub logging: LoggingConfig,

    /// GUI 专属配置
    #[serde(default)]
    pub gui: GuiSettings,

    /// RAG 向量检索配置
    #[serde(default)]
    pub rag: RagConfig,

    /// 本地持久化配置（SQLite via SeaORM）
    #[serde(default)]
    pub storage: GuiStorageConfig,
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
    /// 思考模式配置（适用于支持思考模式的 AI 模型）
    #[serde(default)]
    pub thinking_config: Option<ThinkingConfig>,
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
            prompt_manager: PromptManagerConfig::default(),
            logging: LoggingConfig::default(),
            gui: GuiSettings::default(),
            rag: RagConfig::default(),
            storage: GuiStorageConfig::default(),
        }
    }
}

// ── 本地持久化配置（SeaORM + SQLite） ──

/// 本地持久化配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiStorageConfig {
    /// SQLite 数据库文件路径（推荐相对路径，路径解析复用 try_load 模式）
    #[serde(default = "default_storage_db_path")]
    pub db_path: String,

    /// 启动时打印 schema 概要（仅调试）
    #[serde(default)]
    pub echo_schema: bool,
}

fn default_storage_db_path() -> String {
    "./data/agent-gui.db".to_string()
}

impl Default for GuiStorageConfig {
    fn default() -> Self {
        Self {
            db_path: default_storage_db_path(),
            echo_schema: false,
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
    ///
    /// 加载失败时同时通过 `eprintln!` 输出，因为这是致命错误——日志只写文件，
    /// 用户在 CLI 上看不到 warn，所以必须 stderr 提示一次。
    pub fn load() -> Self {
        match Self::try_load() {
            Ok(cfg) => {
                tracing::info!("配置加载成功: {} 个 AI provider, RAG api_key={}",
                    cfg.ai_providers.len(),
                    if cfg.rag.embedding_api_key.is_empty() { "未配置" } else { "已配置" });
                cfg
            }
            Err(e) => {
                let msg = format!("配置加载失败，使用默认配置: {}", e);
                eprintln!("[planned-agent-gui] {}", msg);
                tracing::warn!("{}", msg);
                Self::default()
            }
        }
    }

    fn try_load() -> Result<Self> {
        // 尝试多个候选路径（Dioxus desktop 运行时 CWD 可能不是 workspace 根）
        let candidates: Vec<std::path::PathBuf> = {
            let mut v = Vec::new();

            // 1. 环境变量覆盖（最高优先级）
            if let Ok(p) = std::env::var("PLANNED_AGENT_CONFIG") {
                v.push(std::path::PathBuf::from(p));
            }

            // 2. 当前工作目录
            if let Ok(cwd) = std::env::current_dir() {
                v.push(cwd.join("config.toml"));
            }

            // 3. 可执行文件所在目录的祖父（target/debug/planned-agent-gui → ../../）
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
                    v.push(dir.join("config.toml"));
                }
            }

            v
        };

        let mut last_err: Option<String> = None;
        for path in &candidates {
            if !path.exists() {
                continue;
            }
            match Self::try_load_from(path) {
                Ok(cfg) => {
                    eprintln!("[planned-agent-gui] 配置加载自: {}", path.display());
                    return Ok(cfg);
                }
                Err(e) => {
                    last_err = Some(format!("{}: {}", path.display(), e));
                }
            }
        }

        // 所有候选路径都失败
        let searched: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
        anyhow::bail!(
            "未找到 config.toml（已尝试 {} 个候选路径: {}），最后一次错误: {}",
            searched.len(),
            searched.join(", "),
            last_err.as_deref().unwrap_or("无")
        )
    }

    fn try_load_from(path: &std::path::Path) -> Result<Self> {
        let cfg = Config::builder()
            .add_source(config::File::from(path))
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
