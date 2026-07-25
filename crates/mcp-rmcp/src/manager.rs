use std::collections::HashMap;
use anyhow::Result;
use planned_agent_core::mcp::McpClient;
use planned_agent_core::types::{Tool, ToolResult, McpServerConfig};
use serde_json::Value;
use tracing::{info, error};
use async_trait::async_trait;
use crate::client::McpClientImpl;
use crate::tools::ToolManager;
use planned_agent_tool_manager::McpManagerTrait;

/// MCP 管理器，管理多个 MCP 服务器
pub struct McpManager {
    clients: HashMap<String, McpClientImpl>,
    tool_manager: ToolManager,
    server_configs: HashMap<String, McpServerConfig>,
}

impl McpManager {
    /// 创建新的 MCP 管理器
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            tool_manager: ToolManager::new(),
            server_configs: HashMap::new(),
        }
    }
    
    /// 连接到单个 MCP 服务器
    pub async fn connect_server(&mut self, config: McpServerConfig) -> Result<()> {
        let name = config.name.clone();
        info!("Connecting to MCP server: {}", name);
        
        let mut client = McpClientImpl::new();
        
        client.connect(config.clone()).await?;
        
        // 获取工具列表
        let tools = client.list_tools().await?;
        
        // 如果有过滤器，只添加指定的工具
        let filtered_tools = if let Some(filter) = &config.tools_filter {
            tools.into_iter()
                .filter(|tool| filter.contains(&tool.name))
                .collect()
        } else {
            tools
        };
        
        self.tool_manager.add_tools(&name, filtered_tools);
        self.clients.insert(name.clone(), client);
        self.server_configs.insert(name.clone(), config);
        
        info!("Connected to MCP server: {} with {} tools", name, self.tool_manager.get_server_tools(&name).len());
        Ok(())
    }
    
    /// 连接到多个 MCP 服务器
    pub async fn connect_all(&mut self, configs: Vec<McpServerConfig>) -> Result<()> {
        for config in configs {
            if let Err(e) = self.connect_server(config).await {
                error!("Failed to connect to MCP server: {}", e);
                // 继续连接其他服务器
            }
        }
        Ok(())
    }
    
    /// 断开单个 MCP 服务器
    pub async fn disconnect_server(&mut self, name: &str) -> Result<()> {
        if let Some(mut client) = self.clients.remove(name) {
            client.disconnect().await?;
            self.tool_manager.remove_server_tools(name);
            self.server_configs.remove(name);
            info!("Disconnected from MCP server: {}", name);
        }
        Ok(())
    }
    
    /// 断开所有 MCP 服务器
    pub async fn disconnect_all(&mut self) -> Result<()> {
        let server_names: Vec<String> = self.clients.keys().cloned().collect();
        for name in server_names {
            self.disconnect_server(&name).await?;
        }
        Ok(())
    }
    
    /// 调用工具（指定服务器）
    pub async fn call_tool(&self, server_name: &str, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        let client = self.clients.get(server_name)
            .ok_or_else(|| anyhow::anyhow!("MCP server not found: {}", server_name))?;
        
        info!("Calling tool '{}' on server '{}'", tool_name, server_name);
        client.call_tool(tool_name, arguments).await
    }
    
    /// 自动路由调用工具（根据工具名称找到对应的服务器）
    pub async fn call_tool_auto(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        let server_name = self.tool_manager.find_server_for_tool(tool_name)
            .ok_or_else(|| anyhow::anyhow!("No server found for tool: {}", tool_name))?;
        
        self.call_tool(&server_name, tool_name, arguments).await
    }
    
    /// 获取所有工具
    pub fn get_all_tools(&self) -> Vec<Tool> {
        self.tool_manager.get_all_tools()
    }
    
    /// 获取指定服务器的工具
    pub fn get_server_tools(&self, server_name: &str) -> Vec<Tool> {
        self.tool_manager.get_server_tools(server_name)
    }
    
    /// 获取所有服务器名称
    pub fn get_server_names(&self) -> Vec<String> {
        self.clients.keys().cloned().collect()
    }
    
    /// 检查服务器是否已连接
    pub fn is_server_connected(&self, server_name: &str) -> bool {
        self.clients.contains_key(server_name)
    }
    
    /// 获取连接状态
    pub async fn get_connection_status(&self) -> HashMap<String, String> {
        let mut status = HashMap::new();
        for (name, client) in &self.clients {
            let client_status = client.connection_status().await;
            status.insert(name.clone(), format!(
                "Connected: {}, Uptime: {}s, Error count: {}",
                client_status.connected, client_status.uptime_secs, client_status.error_count
            ));
        }
        status
    }
}

/// 为 McpManager 实现 McpManagerTrait
#[async_trait]
impl McpManagerTrait for McpManager {
    async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        self.call_tool_auto(tool_name, arguments).await
    }
    
    fn get_all_tools(&self) -> Vec<Tool> {
        self.get_all_tools()
    }
    
    fn find_server_for_tool(&self, tool_name: &str) -> Option<String> {
        self.tool_manager.find_server_for_tool(tool_name)
    }
    
    fn get_server_names(&self) -> Vec<String> {
        self.get_server_names()
    }
    
    fn get_server_categories(&self, server_name: &str) -> Option<Vec<String>> {
        // 从 server_configs 中获取配置
        if let Some(config) = self.server_configs.get(server_name) {
            config.categories.clone()
        } else {
            None
        }
    }
}
