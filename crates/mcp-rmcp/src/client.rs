use async_trait::async_trait;
use anyhow::Result;
use rmcp::{ServiceExt, transport::{TokioChildProcess, ConfigureCommandExt}};
use tokio::process::Command;
use planned_agent_core::{
    mcp::McpClient,
    types::{Tool, ToolResult, McpServerConfig, ConnectionStatus},
};
use serde_json::Value;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{info};

/// MCP 客户端实现
pub struct McpClientImpl {
    client: Option<rmcp::service::RunningService<rmcp::RoleClient, ()>>,
    config: Option<McpServerConfig>,
    connected: bool,
    last_ping: Mutex<Option<Instant>>,
    error_count: u32,
    connection_start: Option<Instant>,
}

impl McpClientImpl {
    /// 创建新的 MCP 客户端
    pub fn new() -> Self {
        Self {
            client: None,
            config: None,
            connected: false,
            last_ping: Mutex::new(None),
            error_count: 0,
            connection_start: None,
        }
    }
    
    /// 转换工具格式
    fn convert_tool(tool: &rmcp::model::Tool) -> Tool {
        Tool {
            name: tool.name.to_string(),
            description: tool.description.clone().unwrap_or_default().to_string(),
            input_schema: serde_json::to_value(&tool.input_schema).unwrap_or_default(),
        }
    }
    
    /// 转换工具结果
    fn convert_tool_result(result: rmcp::model::CallToolResult) -> ToolResult {
        let content = result.content.first()
            .map(|c| {
                // 根据内容类型转换
                if let Some(text) = c.as_text() {
                    Value::String(text.text.clone())
                } else if let Some(_) = c.as_image() {
                    Value::String("[Image]".to_string())
                } else if let Some(_) = c.as_resource() {
                    Value::String("[Resource]".to_string())
                } else {
                    Value::String("[Unknown content type]".to_string())
                }
            })
            .unwrap_or(Value::Null);
        
        ToolResult {
            call_id: String::new(), // MCP 不提供 call_id
            content,
            is_error: result.is_error.unwrap_or(false),
        }
    }
}

#[async_trait]
impl McpClient for McpClientImpl {
    async fn connect(&mut self, config: McpServerConfig) -> Result<()> {
        info!("Connecting to MCP server: {}", config.server_command);
        
        let transport = TokioChildProcess::new(Command::new(&config.server_command).configure(|cmd| {
            for arg in &config.server_args {
                cmd.arg(arg);
            }
        }))?;
        
        let client = ().serve(transport).await?;
        
        self.client = Some(client);
        self.config = Some(config);
        self.connected = true;
        self.connection_start = Some(Instant::now());
        
        info!("Successfully connected to MCP server");
        Ok(())
    }
    
    async fn list_tools(&self) -> Result<Vec<Tool>> {
        let client = self.client.as_ref()
            .ok_or_else(|| anyhow::anyhow!("MCP client not connected"))?;
        
        let tools = client.list_all_tools().await?;
        
        Ok(tools.iter().map(|t| Self::convert_tool(t)).collect())
    }
    
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult> {
        let client = self.client.as_ref()
            .ok_or_else(|| anyhow::anyhow!("MCP client not connected"))?;
        
        // 将 Value 转换为 JsonObject
        let arguments_map = match arguments {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        
        let params = rmcp::model::CallToolRequestParams {
            meta: None,
            name: name.to_string().into(),
            arguments: Some(arguments_map),
            task: None,
        };
        
        let result = client.call_tool(params).await?;
        
        Ok(Self::convert_tool_result(result))
    }
    
    async fn disconnect(&mut self) -> Result<()> {
        info!("Disconnecting from MCP server");
        
        self.client = None;
        self.connected = false;
        self.connection_start = None;
        
        info!("Disconnected from MCP server");
        Ok(())
    }
    
    async fn is_connected(&self) -> bool {
        self.connected
    }
    
    async fn connection_status(&self) -> ConnectionStatus {
        let last_ping = *self.last_ping.lock().await;
        
        ConnectionStatus {
            connected: self.connected,
            last_ping: last_ping.map(|instant| {
                chrono::Utc::now() - chrono::Duration::from_std(instant.elapsed()).unwrap_or_default()
            }),
            error_count: self.error_count,
            uptime_secs: self.connection_start
                .map(|start| start.elapsed().as_secs())
                .unwrap_or(0),
        }
    }
    
    async fn ping(&self) -> Result<()> {
        let client = self.client.as_ref()
            .ok_or_else(|| anyhow::anyhow!("MCP client not connected"))?;
        
        // MCP 没有直接的 ping 方法，我们可以尝试列出工具来测试连接
        let _ = client.list_all_tools().await?;
        
        let mut last_ping = self.last_ping.lock().await;
        *last_ping = Some(Instant::now());
        
        Ok(())
    }
    
    async fn reconnect(&mut self) -> Result<()> {
        info!("Reconnecting to MCP server");
        
        // 先断开
        self.disconnect().await?;
        
        // 重新连接
        if let Some(config) = self.config.clone() {
            self.connect(config).await?;
        } else {
            return Err(anyhow::anyhow!("No MCP configuration available for reconnection"));
        }
        
        Ok(())
    }
}
