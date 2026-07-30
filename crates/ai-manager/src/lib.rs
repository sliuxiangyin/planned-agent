use std::collections::HashMap;
use std::sync::Arc;
use planned_agent_core::ai::AiClient;
use planned_agent_core::types::AiProviderConfig;
use planned_agent_ai_openai::{OpenAiClient, OpenAiClientConfig};

/// AI客户端管理器
#[derive(Clone)]
pub struct AiManager {
    clients: HashMap<String, Arc<dyn AiClient>>,
    default_name: Option<String>,
}

impl AiManager {
    /// 从配置初始化AI管理器
    pub fn from_config(configs: Vec<AiProviderConfig>) -> anyhow::Result<Self> {
        let mut clients = HashMap::new();
        let mut default_name = None;
        
        for config in configs {
            let client: Arc<dyn AiClient> = match config.provider.as_str() {
                "openai" => {
                    let client_config = OpenAiClientConfig {
                        api_key: config.api_key.clone(),
                        model: config.model.clone(),
                        base_url: config.base_url.clone(),
                        default_temperature: config.temperature,
                        default_max_tokens: config.max_tokens,
                        organization: None,
                        thinking_config: config.thinking_config.clone(),
                    };
                    Arc::new(OpenAiClient::new(client_config))
                }
                _ => return Err(anyhow::anyhow!("Unsupported AI provider: {}", config.provider)),
            };
            
            if config.is_default {
                default_name = Some(config.name.clone());
            }
            
            clients.insert(config.name.clone(), client);
        }
        
        Ok(Self { clients, default_name })
    }
    
    /// 获取默认的AI客户端
    pub fn default(&self) -> anyhow::Result<Arc<dyn AiClient>> {
        self.default_name.as_ref()
            .and_then(|name| self.clients.get(name))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No default AI provider configured"))
    }
    
    /// 获取指定名称的AI客户端
    pub fn get(&self, name: &str) -> anyhow::Result<Arc<dyn AiClient>> {
        self.clients.get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("AI provider not found: {}", name))
    }
    
    /// 获取所有提供商名称
    pub fn provider_names(&self) -> Vec<String> {
        self.clients.keys().cloned().collect()
    }
    
    /// 检查是否有默认提供商
    pub fn has_default(&self) -> bool {
        self.default_name.is_some()
    }
    
    /// 获取提供商数量
    pub fn provider_count(&self) -> usize {
        self.clients.len()
    }
}
