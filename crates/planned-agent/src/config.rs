use serde::{Deserialize, Serialize};
use planned_agent_core::types::{AiProviderConfig, McpServerConfig};
use planned_agent_prompt_manager::PromptManagerConfig;
use anyhow::Result;
use config::Config;

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
    
    pub logging: LoggingConfig,
}

/// 日志配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
}

impl AppConfig {
    /// 从配置文件加载配置
    pub fn load() -> Result<Self> {
        let config = Config::builder()
            .add_source(config::File::with_name("config"))
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
                    timeout_secs: Some(30),
                    max_retries: Some(3),
                    is_default: true,
                    tools_filter: None,
                    categories: Some(vec!["Browser".to_string()]),
                },
            ],
            prompt_manager: PromptManagerConfig::default(),
            logging: LoggingConfig {
                level: "info".to_string(),
                format: "pretty".to_string(),
            },
        }
    }
}
