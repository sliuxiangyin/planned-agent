use async_trait::async_trait;
use anyhow::Result;
use serde_json::Value;
use crate::mcp::types::{Tool, ToolResult, McpServerConfig, ConnectionStatus};

/// MCP 客户端 trait
#[async_trait]
pub trait McpClient: Send + Sync {
    /// 连接到 MCP 服务器
    async fn connect(&mut self, config: McpServerConfig) -> Result<()>;
    
    /// 列出可用工具
    async fn list_tools(&self) -> Result<Vec<Tool>>;
    
    /// 调用工具
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult>;
    
    /// 断开连接
    async fn disconnect(&mut self) -> Result<()>;
    
    /// 检查连接是否健康
    async fn is_connected(&self) -> bool;
    
    /// 获取连接状态信息
    async fn connection_status(&self) -> ConnectionStatus;
    
    /// 心跳检测（如果服务器支持）
    async fn ping(&self) -> Result<()>;
    
    /// 重新连接
    async fn reconnect(&mut self) -> Result<()>;
}
