use planned_agent_core::mcp::types::McpServerConfig;
use planned_agent_core::mcp::McpClient;
use planned_agent_mcp_rmcp::McpClientImpl;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // 创建 MCP 配置
    let config = McpServerConfig {
        name: "everything".to_string(),
        server_command: "npx".to_string(),
        server_args: vec![
            "-y".to_string(),
            "@modelcontextprotocol/server-everything".to_string(),
        ],
        transport: "stdio".to_string(),
        timeout_secs: Some(30),
        max_retries: Some(3),
        is_default: true,
        tools_filter: None,
        categories: None,
    };

    // 创建 MCP 客户端
    let mut client = McpClientImpl::new();

    println!("Connecting to MCP server...");

    // 连接到服务器
    client.connect(config).await?;

    println!("Connected! Listing tools...");

    // 列出工具
    let tools = client.list_tools().await?;

    println!("Available tools:");
    for tool in &tools {
        println!("  - {}: {}", tool.name, tool.description);
    }

    // 测试连接状态
    let status = client.connection_status().await;
    println!("\nConnection status:");
    println!("  Connected: {}", status.connected);
    println!("  Uptime: {} seconds", status.uptime_secs);
    println!("  Error count: {}", status.error_count);

    // 测试 ping
    println!("\nTesting ping...");
    client.ping().await?;
    println!("Ping successful!");

    // 断开连接
    println!("\nDisconnecting...");
    client.disconnect().await?;
    println!("Disconnected!");

    Ok(())
}
