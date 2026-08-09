use serde::{Deserialize, Serialize};
use planned_agent_core::ai::config::AiProviderConfig;
use planned_agent_core::mcp::types::McpServerConfig;
use planned_agent_prompt_manager::PromptManagerConfig;
use anyhow::Result;
use config::Config;
use std::path::Path;

/// 应用配置（支持多配置）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 新版多AI提供商配置
    #[serde(default)]
    pub ai_providers: Vec<AiProviderConfig>,

    /// 新版多MCP服务器配置
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,

    /// Prompt管理器配置
    #[serde(default)]
    pub prompt_manager: PromptManagerConfig,

    /// 轨迹记录系统配置
    #[serde(default)]
    pub trace: TraceConfig,

    pub logging: LoggingConfig,
}

/// 轨迹记录系统配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceConfig {
    /// 是否启用
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 存储目录
    #[serde(default = "default_trace_dir")]
    pub storage_dir: String,
    /// 入库质量门槛
    #[serde(default = "default_max_iter")]
    pub max_iterations_for_record: usize,
    /// 是否使用 LLM 泛化
    #[serde(default = "default_true")]
    pub use_llm_generalization: bool,
    /// 泛化使用的模型名称（空 = 默认 AI）
    #[serde(default)]
    pub generalization_model: String,
}

fn default_true() -> bool { true }
fn default_trace_dir() -> String { "./traces".to_string() }
fn default_max_iter() -> usize { 5 }

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            storage_dir: "./traces".to_string(),
            max_iterations_for_record: 5,
            use_llm_generalization: true,
            generalization_model: String::new(),
        }
    }
}

/// 日志配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
}

impl AppConfig {
    /// 从指定的配置文件路径加载配置。
    ///
    /// 调用方负责决定路径来源（CLI 参数、默认值、环境变量等），
    /// 本方法不再做任何目录探测或回退查找 —— 路径不存在或解析失败会直接返回错误。
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let config = Config::builder()
            .add_source(config::File::from(path.as_ref()))
            .build()?;

        let app_config: AppConfig = config.try_deserialize()?;

        Ok(app_config)
    }

    
    /// 获取默认AI提供商配置
    pub fn get_default_ai_provider(&self) -> Option<&AiProviderConfig> {
        self.ai_providers.iter().find(|p| p.is_default)
            .or_else(|| self.ai_providers.first())
    }
    
    /// 根据名称获取AI提供商配置
    pub fn get_ai_provider(&self, name: &str) -> Option<&AiProviderConfig> {
        self.ai_providers.iter().find(|p| p.name == name)
    }
    
    /// 获取默认MCP服务器配置
    pub fn get_default_mcp_server(&self) -> Option<&McpServerConfig> {
        self.mcp_servers.iter().find(|s| s.is_default)
            .or_else(|| self.mcp_servers.first())
    }
    
    /// 根据名称获取MCP服务器配置
    pub fn get_mcp_server(&self, name: &str) -> Option<&McpServerConfig> {
        self.mcp_servers.iter().find(|s| s.name == name)
    }
    
    /// 获取所有AI提供商名称
    pub fn get_ai_provider_names(&self) -> Vec<String> {
        self.ai_providers.iter().map(|p| p.name.clone()).collect()
    }
    
    /// 获取所有MCP服务器名称
    pub fn get_mcp_server_names(&self) -> Vec<String> {
        self.mcp_servers.iter().map(|s| s.name.clone()).collect()
    }
    
    /// 创建默认配置
    pub fn default_config() -> Self {
        Self {
            ai_providers: vec![
                AiProviderConfig {
                    name: "openai".to_string(),
                    provider: "openai".to_string(),
                    api_key: "".to_string(),
                    model: "gpt-4".to_string(),
                    max_tokens: Some(4096),
                    temperature: Some(0.7),
                    base_url: None,
                    is_default: true,
                    thinking_config: None,
                },
            ],
            mcp_servers: vec![
                McpServerConfig {
                    name: "playwright".to_string(),
                    server_command: "npx".to_string(),
                    server_args: vec!["@playwright/mcp@latest".to_string()],
                    transport: "stdio".to_string(),
                    // 覆盖完整冷启动链：spawn npx → 首次拉包（可达数十秒）→ MCP initialize 握手
                    timeout_secs: Some(120),
                    handshake_timeout_secs: None,
                    max_retries: Some(3),
                    is_default: true,
                    tools_filter: None,
                    categories: Some(vec!["Browser".to_string()]),
                },
            ],
            prompt_manager: PromptManagerConfig::default(),
            trace: TraceConfig::default(),
            logging: LoggingConfig {
                level: "info".to_string(),
                format: "pretty".to_string(),
            },
        }
    }
}
