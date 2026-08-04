use async_trait::async_trait;
use anyhow::Result;
use rmcp::{ServiceExt, transport::{TokioChildProcess, ConfigureCommandExt}};
use tokio::io::AsyncReadExt;
use tokio::process::{ChildStderr, Command};
use std::process::Stdio;
use planned_agent_core::{
    mcp::McpClient,
    types::{Tool, ToolResult, McpServerConfig, ConnectionStatus, ConnectionError},
};
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// 读取子进程 stderr 末尾内容的超时（毫秒）。
///
/// 当子进程崩溃后，我们尝试从捕获的 stderr 管道中读取剩余数据。
/// 这里限制 500ms 以免阻塞过久——大多数情况下子进程已退出/EOF。
const STDERR_DRAIN_TIMEOUT_MS: u64 = 500;

/// 排空子进程 stderr 管道，返回剩余内容（如果有）。
///
/// 仅返回**非空**结果；空 stderr 会返回 None 以避免污染 UI 显示。
async fn drain_stderr(stderr: Option<ChildStderr>) -> Option<String> {
    let mut stderr = stderr?;
    let mut buf = String::new();
    let _ = tokio::time::timeout(
        Duration::from_millis(STDERR_DRAIN_TIMEOUT_MS),
        stderr.read_to_string(&mut buf),
    )
    .await;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// MCP 冷启动链路的默认超时上限（秒）。
///
/// 覆盖完整链：spawn 子进程 → npx 拉包（首次可达数十秒）→ 真实 MCP server 启动 → initialize 握手。
/// 该常量是 `McpServerConfig::timeout_secs` 为 None 时的兜底。
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 120;

/// MCP 客户端实现
pub struct McpClientImpl {
    client: Option<rmcp::service::RunningService<rmcp::RoleClient, ()>>,
    config: Option<McpServerConfig>,
    connected: bool,
    last_ping: Mutex<Option<Instant>>,
    error_count: u32,
    connection_start: Option<Instant>,
    /// 最近一次连接失败的结构化错误（成功连接后会被清空）
    last_error: Mutex<Option<ConnectionError>>,
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
            last_error: Mutex::new(None),
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

    /// 取出最近一次连接失败的结构化错误（不影响内部状态）
    ///
    /// 供 `fetch_and_cache_tools` 等上层在 connect 失败后，
    /// 将 `ConnectionError` 透传到 UI 层展示。
    pub async fn take_last_error(&self) -> Option<ConnectionError> {
        self.last_error.lock().await.clone()
    }
}

#[async_trait]
impl McpClient for McpClientImpl {
    async fn connect(&mut self, config: McpServerConfig) -> Result<()> {
        info!("Connecting to MCP server: {} ({:?})", config.server_command, config.server_args);

        // 1. 读取/兜底超时上限
        let timeout_secs = config.timeout_secs.unwrap_or(DEFAULT_CONNECT_TIMEOUT_SECS);

        // 2. spawn 子进程（npx / node / python 等）
        //    这一步的失败通常是命令不存在或权限问题，单独捕获以便于 UI 区分
        //
        // 注意：这里用 builder 而非 new()，目的是把 stderr 接管为 Stdio::piped()，
        //       这样子进程（如 npx / pdf-lib）崩溃时的真实报错能被我们读取并展示给用户，
        //       而不是仅仅通过 rmcp 的"connection closed"二手消息告诉用户。
        let (transport, stderr_handle) = match TokioChildProcess::builder(
            Command::new(&config.server_command).configure(|cmd| {
                for arg in &config.server_args {
                    cmd.arg(arg);
                }
            }),
        )
        .stderr(Stdio::piped())
        .spawn()
        {
            Ok(t) => t,
            Err(e) => {
                let reason = e.to_string();
                let err = ConnectionError::Spawn { reason: reason.clone() };
                warn!("Failed to spawn MCP server '{}': {}", config.name, reason);
                *self.last_error.lock().await = Some(err);
                anyhow::bail!(
                    "Failed to spawn MCP server '{}' (command: {}): {}",
                    config.name, config.server_command, reason
                );
            }
        };

        // 3. 等待 MCP initialize 握手完成
        //    这一段覆盖：npx 拉包（首次可达数十秒）→ 真实 MCP server 启动 → initialize 握手
        //    用 tokio::time::timeout 强制兜底，避免无限挂起
        let started = Instant::now();
        let serve_result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            ().serve(transport),
        )
        .await;

        // 无论成败，都尝试排空 stderr —— 拿到子进程崩溃时的真实错误（如 MODULE_NOT_FOUND）
        let stderr_tail = drain_stderr(stderr_handle).await;

        let client = match serve_result {
            Ok(Ok(client)) => client,
            Ok(Err(e)) => {
                // 进程启动成功但 MCP 协议握手失败
                // —— 常见原因是子进程在握手前就崩溃了（如 npm 包内部 require 失败）
                let elapsed = started.elapsed().as_secs();
                let reason = e.to_string();
                let err = ConnectionError::Handshake {
                    reason: reason.clone(),
                    stderr_tail: stderr_tail.clone(),
                };
                warn!(
                    "MCP handshake failed for '{}' after {}s: {}{}",
                    config.name,
                    elapsed,
                    reason,
                    stderr_tail
                        .as_deref()
                        .map(|s| format!("\n  stderr: {}", s.replace('\n', "\n  ")))
                        .unwrap_or_default(),
                );
                *self.last_error.lock().await = Some(err);
                anyhow::bail!(
                    "MCP handshake failed for '{}' after {}s: {}",
                    config.name, elapsed, reason
                );
            }
            Err(_elapsed) => {
                // 超时：spawn 后等到 timeout_secs 仍未完成 initialize
                let actual = started.elapsed().as_secs();
                let err = ConnectionError::Timeout {
                    elapsed_secs: actual,
                    timeout_secs,
                    stderr_tail: stderr_tail.clone(),
                };
                warn!(
                    "MCP server '{}' startup timed out after {}s (limit {}s){}",
                    config.name,
                    actual,
                    timeout_secs,
                    stderr_tail
                        .as_deref()
                        .map(|s| format!("\n  stderr: {}", s.replace('\n', "\n  ")))
                        .unwrap_or_default(),
                );
                *self.last_error.lock().await = Some(err);
                anyhow::bail!(
                    "MCP server '{}' startup timed out after {}s (limit {}s). \
                     npx package download or handshake may be slow; \
                     consider raising `timeout_secs` in the server config.",
                    config.name, actual, timeout_secs
                );
            }
        };

        // 4. 成功：写入状态并清空历史错误
        self.client = Some(client);
        self.config = Some(config);
        self.connected = true;
        self.connection_start = Some(Instant::now());
        *self.last_error.lock().await = None;

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
        let last_error = self.last_error.lock().await.clone();

        ConnectionStatus {
            connected: self.connected,
            last_ping: last_ping.map(|instant| {
                chrono::Utc::now() - chrono::Duration::from_std(instant.elapsed()).unwrap_or_default()
            }),
            error_count: self.error_count,
            uptime_secs: self.connection_start
                .map(|start| start.elapsed().as_secs())
                .unwrap_or(0),
            last_error,
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
