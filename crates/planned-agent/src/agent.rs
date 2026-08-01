use planned_agent_core::{
    ai::AiClient,
    types::{ChatCompletionRequest, Message, MessageRole, MessageContent, ToolCall, ToolType, ToolDefinition, FunctionDefinition, AiProviderConfig, McpServerConfig},
};
use planned_agent_ai_manager::AiManager;
use planned_agent_mcp_rmcp::McpManager;
use planned_agent_prompt_manager::{FilePromptManager, PromptManagerConfig};
use planned_agent_tool_manager::ToolRegistry;
use planned_agent_tool_manager::builtin::file_tools::FileToolsProvider;
use planned_agent_tool_manager::builtin::text_tools::TextToolsProvider;
use planned_agent_tool_manager::builtin::system_tools::SystemToolsProvider;
use planned_agent_tool_manager::builtin::data_tools::DataToolsProvider;
use planned_agent_tool_manager::builtin::ai_tools::AiToolsProvider;
use planned_agent_tool_manager::builtin::web_tools::WebToolsProvider;
use planned_agent::planner::react::chunk::ChunkToolsProvider;
use planned_agent::planner::react::chunk::executor_context::ExecutorContext;
use anyhow::Result;
use tracing::{info, error};
use std::collections::HashMap;
use std::sync::Arc;

/// 代理核心（支持多配置）
pub struct Agent {
    ai_manager: Option<AiManager>,
    tool_registry: Arc<ToolRegistry>,
    prompt_manager: Option<FilePromptManager>,
    /// Executor 运行时注入上下文（避免与 ToolRegistry 循环引用）
    exec_ctx: Arc<ExecutorContext>,
}

impl Agent {
    /// 创建新的代理
    pub fn new() -> Self {
        let tool_registry = Arc::new(ToolRegistry::new());
        let exec_ctx = Arc::new(ExecutorContext::new());

        // 注册内置工具
        tool_registry.register_builtin_provider(&FileToolsProvider);
        tool_registry.register_builtin_provider(&TextToolsProvider);
        tool_registry.register_builtin_provider(&SystemToolsProvider);
        tool_registry.register_builtin_provider(&DataToolsProvider);
        tool_registry.register_builtin_provider(&AiToolsProvider);
        tool_registry.register_builtin_provider(&WebToolsProvider);
        let chunk_provider = ChunkToolsProvider::new(exec_ctx.clone());
        tool_registry.register_builtin_provider(&chunk_provider);

        Self {
            ai_manager: None,
            tool_registry,
            prompt_manager: None,
            exec_ctx,
        }
    }
    
    /// 初始化 Prompt 管理器
    pub async fn init_prompt_manager(&mut self, config: PromptManagerConfig) -> Result<()> {
        info!("Initializing prompt manager with config: {:?}", config);
        info!("Prompt directory: {:?}", config.prompt_dir);
        let manager: FilePromptManager = FilePromptManager::new(config)?;
        manager.initialize().await?;
        self.prompt_manager = Some(manager);
        info!("Prompt manager initialized");
        Ok(())
    }
    
    /// 获取 Prompt 管理器
    pub fn get_prompt_manager(&self) -> Option<&FilePromptManager> {
        self.prompt_manager.as_ref()
    }
    
    /// 获取 Prompt 管理器的 Arc 引用
    pub fn get_prompt_manager_arc(&self) -> Option<Arc<FilePromptManager>> {
        self.prompt_manager.as_ref().map(|pm| Arc::new(pm.clone()))
    }
    
    /// 初始化 AI 客户端
    pub fn init_ai_clients(&mut self, configs: Vec<AiProviderConfig>) -> Result<()> {
        self.ai_manager = Some(AiManager::from_config(configs)?);
        info!("Initialized AI manager");
        Ok(())
    }
    
    /// 连接到多个 MCP 服务器
    pub async fn connect_mcp_servers(&mut self, configs: Vec<McpServerConfig>) -> Result<()> {
        // 创建 MCP 管理器并连接服务器（带超时）
        let mut mcp_manager = McpManager::new();
        
        // 使用 tokio::time::timeout 添加超时
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            mcp_manager.connect_all(configs)
        ).await {
            Ok(result) => result?,
            Err(_) => {
                error!("MCP connection timeout after 10 seconds");
                return Err(anyhow::anyhow!("MCP connection timeout"));
            }
        }
        
        // 获取连接状态
        let stats = mcp_manager.get_connection_status().await;
        for (name, status) in stats {
            info!("MCP server '{}': {}", name, status);
        }
        
        // 将 MCP 管理器注入到工具注册表
        let mcp_manager_arc = Arc::new(mcp_manager);
        self.tool_registry.set_mcp_manager(mcp_manager_arc);
        
        Ok(())
    }
    
    /// 处理用户输入（使用默认 AI 提供商）
    pub async fn process_input(&mut self, input: &str) -> Result<String> {
        self.process_input_with_provider(input, Some("siliconflow-Qwen3.5-4B")).await
    }
    
    /// 处理用户输入（指定 AI 提供商）
    pub async fn process_input_with_provider(&mut self, input: &str, provider_name: Option<&str>) -> Result<String> {
        info!("Processing input: {}", input);
        
        // 1. 获取 AI 客户端
        let ai_client = self.get_ai_client(provider_name)?;
        
        // 2. 获取所有工具定义
        let mcp_tools = self.tool_registry.get_all_tools();
        
        // 3. 转换工具定义
        let tools: Vec<ToolDefinition> = mcp_tools.iter().map(|t| {
            ToolDefinition {
                r#type: ToolType::Function,
                function: FunctionDefinition {
                    name: t.name.clone(),
                    description: Some(t.description.clone()),
                    parameters: Some(t.input_schema.clone()),
                    strict: None,
                },
            }
        }).collect();
        
        // 4. 发送给 AI（带工具定义）
        let request = ChatCompletionRequest {
            model: ai_client.model_name().to_string(),
            messages: vec![
                Message {
                    role: MessageRole::User,
                    content: Some(MessageContent::Text {
                        text: input.to_string(),
                    }),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                }
            ],
            tools: Some(tools),
            temperature: None,
            max_tokens: None,
            stream: false,
            extra: HashMap::new(),
        };
        
        let response = ai_client.chat_completion(request).await?;
        
        // 5. 处理工具调用
        if let Some(choice) = response.choices.first() {
            if let Some(tool_calls) = &choice.message.tool_calls {
                return self.handle_tool_calls(tool_calls.clone(), input, provider_name).await;
            }
            
            // 6. 返回响应
            if let Some(MessageContent::Text { text }) = &choice.message.content {
                return Ok(text.clone());
            }
        }
        
        Ok(String::new())
    }
    
    /// 流式处理用户输入
    pub async fn process_input_stream(&mut self, input: &str) -> Result<planned_agent_ai_openai::StreamResult> {
        self.process_input_stream_with_provider(input, None).await
    }
    
    /// 流式处理用户输入（指定 AI 提供商）
    pub async fn process_input_stream_with_provider(&mut self, input: &str, provider_name: Option<&str>) -> Result<planned_agent_ai_openai::StreamResult> {
        info!("Processing input with streaming: {}", input);
        
        // 1. 获取 AI 客户端
        let ai_client = self.get_ai_client(provider_name)?;
        
        // 2. 获取所有工具定义
        let mcp_tools = self.tool_registry.get_all_tools();
        
        // 3. 转换工具定义
        let tools: Vec<ToolDefinition> = mcp_tools.iter().map(|t| {
            ToolDefinition {
                r#type: ToolType::Function,
                function: FunctionDefinition {
                    name: t.name.clone(),
                    description: Some(t.description.clone()),
                    parameters: Some(t.input_schema.clone()),
                    strict: None,
                },
            }
        }).collect();
        
        // 4. 发送给 AI（带工具定义）
        let request = ChatCompletionRequest {
            model: ai_client.model_name().to_string(),
            messages: vec![
                Message {
                    role: MessageRole::User,
                    content: Some(MessageContent::Text {
                        text: input.to_string(),
                    }),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                }
            ],
            tools: Some(tools),
            temperature: None,
            max_tokens: None,
            stream: true,
            extra: HashMap::new(),
        };
        
        let stream_response = ai_client.chat_completion_stream(request).await?;
        
        // 5. 处理流式响应
        let stream_result = planned_agent_ai_openai::StreamHandler::collect_stream(stream_response.stream).await?;
        
        Ok(stream_result)
    }
    
    /// 处理工具调用
    async fn handle_tool_calls(&mut self, tool_calls: Vec<ToolCall>, original_input: &str, provider_name: Option<&str>) -> Result<String> {
        info!("Handling {} tool calls", tool_calls.len());
        
        let mut results = Vec::new();
        
        for call in tool_calls {
            info!("Calling tool: {}", call.function.name);
            
            // 使用工具注册表调用工具（自动路由）
            let outcome = self.tool_registry.call_tool(&call.function.name, serde_json::from_str(&call.function.arguments).unwrap_or_default()).await?;
            results.push((call, outcome));
        }

        // 将工具结果反馈给 AI
        let tool_results_text = results.iter()
            .map(|(call, outcome)| {
                format!("Tool '{}' result: {}", call.function.name, outcome.result.content)
            })
            .collect::<Vec<_>>()
            .join("\n");
        
        let ai_client = self.get_ai_client(provider_name)?;
        
        let follow_up_request = ChatCompletionRequest {
            model: ai_client.model_name().to_string(),
            messages: vec![
                Message {
                    role: MessageRole::User,
                    content: Some(MessageContent::Text {
                        text: format!(
                            "Original question: {}\n\nTool results:\n{}\n\nPlease provide a final answer based on the tool results.",
                            original_input, tool_results_text
                        ),
                    }),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                }
            ],
            tools: None,
            temperature: None,
            max_tokens: None,
            stream: false,
            extra: HashMap::new(),
        };
        
        let final_response = ai_client.chat_completion(follow_up_request).await?;
        
        if let Some(choice) = final_response.choices.first() {
            if let Some(MessageContent::Text { text }) = &choice.message.content {
                return Ok(text.clone());
            }
        }
        
        Ok(String::new())
    }
    
    /// 获取 AI 客户端
    fn get_ai_client(&self, provider_name: Option<&str>) -> Result<Arc<dyn AiClient>> {
        let manager = self.ai_manager.as_ref()
            .ok_or_else(|| anyhow::anyhow!("AI manager not initialized"))?;
        
        match provider_name {
            Some(name) => manager.get(name),
            None => manager.default(),
        }
    }
    
    /// 获取工具注册表状态
    pub fn get_tool_registry_status(&self) -> String {
        let stats = self.tool_registry.get_stats();
        format!(
            "Tool Registry Status:\n  Total: {}\n  Enabled: {}\n  Disabled: {}\n  MCP: {}\n  Custom: {}\n  Builtin: {}",
            stats.total, stats.enabled, stats.disabled, stats.mcp_count, stats.custom_count, stats.builtin_count
        )
    }
    
    /// 获取可用工具列表
    pub fn get_available_tools(&self) -> Vec<String> {
        self.tool_registry.get_all_tools().iter().map(|t| t.name.clone()).collect()
    }
    
    /// 获取可用工具列表（包含分类信息）
    pub fn get_available_tools_with_categories(&self) -> Vec<(String, Vec<String>)> {
        self.tool_registry.get_all_tools().iter().map(|t| {
            let categories = self.tool_registry.get_metadata(&t.name)
                .map(|m| m.categories.iter().map(|c| {
                    // 使用"分类名：工具描述"格式
                    let desc: String = if t.description.is_empty() {
                        c.description().to_string()
                    } else {
                        format!("{}：{}", c.description(), t.description)
                    };
                    desc
                }).collect())
                .unwrap_or_default();
            (t.name.clone(), categories)
        }).collect()
    }
    
    /// 获取所有 AI 提供商名称
    pub fn get_ai_provider_names(&self) -> Vec<String> {
        self.ai_manager.as_ref()
            .map(|m| m.provider_names())
            .unwrap_or_default()
    }
    
    /// 获取所有 MCP 服务器名称
    pub fn get_mcp_server_names(&self) -> Vec<String> {
        // 从工具注册表中获取MCP工具的服务器名称
        let mcp_tools = self.tool_registry.get_tools_by_source("mcp");
        let mut server_names: Vec<String> = mcp_tools.iter()
            .filter_map(|tool| {
                if let Some(metadata) = self.tool_registry.get_metadata(&tool.name) {
                    if let planned_agent_core::tool_registry::ToolSource::Mcp { server_name } = metadata.source {
                        Some(server_name)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();
        server_names.sort();
        server_names.dedup();
        server_names
    }
    
    /// 获取 AI 管理器
    pub fn get_ai_manager(&self) -> Option<&AiManager> {
        self.ai_manager.as_ref()
    }
    
    /// 获取工具注册表
    pub fn get_tool_registry(&self) -> &Arc<ToolRegistry> {
        &self.tool_registry
    }

    /// 获取执行器注入上下文
    pub fn get_exec_ctx(&self) -> &Arc<ExecutorContext> {
        &self.exec_ctx
    }
}
