use async_trait::async_trait;
use anyhow::Result;
use rmcp::{ServiceExt, transport::{TokioChildProcess, ConfigureCommandExt}};
use tokio::io::AsyncReadExt;
use tokio::process::{ChildStderr, Command};
use std::process::Stdio;
use planned_agent_core::{
    mcp::{types::{Tool, ToolResult, McpServerConfig, ConnectionStatus, ConnectionError}, McpClient},
};
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::command_resolver::resolve_command;

/// 读取子进程 stderr 末尾内容的超时（毫秒）。
///
/// 当子进程崩溃后，我们尝试从捕获的 stderr 管道中读取剩余数据。
/// 这里限制 500ms 以免阻塞过久——大多数情况下子进程已退出/EOF。
const STDERR_DRAIN_TIMEOUT_MS: u64 = 500;

/// 排空子进程 stderr 管道，返回剩余内容（如果有）。
///
/// `prefix` 是 stderr 探测阶段已被消费的前缀字节（如探测"进程是否激活"
/// 时读走的首字节），先回填再读取剩余，避免首字符丢失
/// （如 `MODULE_NOT_FOUND` 变成 `ODULE_NOT_FOUND`）。
///
/// 仅返回**非空**结果；空 stderr 会返回 None 以避免污染 UI 显示。
async fn drain_stderr(stderr: Option<ChildStderr>, prefix: &[u8]) -> Option<String> {
    let mut buf = String::from_utf8_lossy(prefix).into_owned();
    if let Some(mut stderr) = stderr {
        let mut rest = String::new();
        let _ = tokio::time::timeout(
            Duration::from_millis(STDERR_DRAIN_TIMEOUT_MS),
            stderr.read_to_string(&mut rest),
        )
        .await;
        buf.push_str(&rest);
    }
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// MCP 冷启动链路的默认超时上限（秒）。
///
/// 覆盖完整链：spawn 子进程 → npx 拉包（首次可达数十秒）→ MCP initialize 握手。
/// 该常量是 `McpServerConfig::timeout_secs` 为 None 时的兜底。
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 180;

/// 握手阶段默认超时（秒）：子进程**无任何输出**时的提前失败线。
///
/// 一旦子进程 stderr 产生输出（进程已激活：npx 开始拉包 / server 打日志），
/// 说明进程活着，改由 [`DEFAULT_CONNECT_TIMEOUT_SECS`] 耐心等待拉包完成；
/// 若进程始终无输出（疑似卡死 / 静默失败），则在此时间内快速失败。
pub const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 30;

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
                } else if c.as_image().is_some() {
                    Value::String("[Image]".to_string())
                } else if c.as_resource().is_some() {
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

        // 1. 读取/兜底超时上限（两级，见 McpServerConfig 字段文档）
        let timeout_secs = config.timeout_secs.unwrap_or(DEFAULT_CONNECT_TIMEOUT_SECS);
        let handshake_timeout_secs = config
            .handshake_timeout_secs
            .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_SECS);

        // 2. spawn 子进程（npx / node / python 等）
        //    这一步的失败通常是命令不存在或权限问题，单独捕获以便于 UI 区分
        //
        // 注意：spawn 前先按当前系统环境解析命令名，而不是直接 `Command::new(命令名)`：
        // - Windows 下 `npx` 实为 `npx.cmd`（无 npx.exe），直接 spawn "npx" 会得到
        //   模糊的 "program not found"；解析后拿到 npx.cmd 完整路径，std 识别
        //   `.cmd`/`.bat` 会自动经 cmd.exe 包装执行，Windows 下也能正常拉起。
        // - 命令确实不存在时，返回明确的"命令不存在: xxx"错误，而非裸 io::Error。
        //
        // 注意：这里用 builder 而非 new()，目的是把 stderr 接管为 Stdio::piped()，
        //       这样子进程（如 npx / pdf-lib）崩溃时的真实报错能被我们读取并展示给用户，
        //       而不是仅仅通过 rmcp 的"connection closed"二手消息告诉用户。
        let resolved_command = match resolve_command(&config.server_command) {
            Ok(p) => p,
            Err(msg) => {
                let err = ConnectionError::Spawn { reason: msg.clone() };
                warn!(
                    "MCP server '{}' 启动命令不存在: {} (command: {})",
                    config.name, msg, config.server_command
                );
                *self.last_error.lock().await = Some(err);
                anyhow::bail!(
                    "Failed to spawn MCP server '{}' (command: {}): {}",
                    config.name, config.server_command, msg
                );
            }
        };

        let (transport, mut stderr_handle) = match TokioChildProcess::builder(
            Command::new(&resolved_command).configure(|cmd| {
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
        //
        //    超时拆成两级（rmcp 的 serve 是不可拆分的单 future，无法直接分段）：
        //    - `timeout_secs`（默认 180s）：冷启动**总**上限，全程硬兜底；
        //    - `handshake_timeout_secs`（默认 30s）：子进程**无任何输出**时的提前失败线。
        //      一旦 stderr 出现数据（进程已激活：npx 开始拉包 / server 打日志），
        //      确认进程活着，切换回 `timeout_secs` 耐心等待。
        //    用 tokio::select! 同时监听：serve 完成 / stderr 首个输出 / deadline 到期。
        let started = Instant::now();
        let hard_deadline = started + Duration::from_secs(timeout_secs);
        let mut deadline = hard_deadline
            .min(started + Duration::from_secs(handshake_timeout_secs));

        // serve future（内部完成 spawn → 拉包 → initialize 握手）
        let serve_fut = ().serve(transport);
        tokio::pin!(serve_fut);

        // stderr 探测：只取第一个字节判断"进程是否激活"；
        // serve 结束后仍会 drain 剩余内容作为 stderr_tail。
        let mut probe = [0u8; 1];
        // stderr 探测阶段实际读到的首字节数（供 drain 回填，避免首字符丢失）
        let mut probe_read = 0usize;
        let stderr_probe = async {
            match stderr_handle.as_mut() {
                Some(h) => h.read(&mut probe).await,
                None => Ok(0),
            }
        };
        tokio::pin!(stderr_probe);

        let mut saw_stderr = false;
        let mut stderr_probe_done = false;

        let serve_result = loop {
            // deadline 已到 → 超时
            if Instant::now() >= deadline {
                break None;
            }
            let sleep = tokio::time::sleep(deadline - Instant::now());
            tokio::pin!(sleep);
            tokio::select! {
                _ = &mut sleep => break None,
                res = &mut serve_fut => break Some(res),
                n = &mut stderr_probe, if !saw_stderr && !stderr_probe_done => {
                    match n {
                        Ok(k) if k > 0 => {
                            // 进程已激活：撤销提前失败线，交给总上限兜底
                            saw_stderr = true;
                            probe_read = k;
                            deadline = hard_deadline;
                        }
                        _ => {
                            // EOF（子进程已退出）或无 stderr 管道：不再探测
                            stderr_probe_done = true;
                        }
                    }
                }
            }
        };
        // 释放对 stderr_handle 的借用，以便随后 drain 剩余内容
        drop(stderr_probe);

        // 无论成败，都尝试排空 stderr —— 拿到子进程崩溃时的真实错误（如 MODULE_NOT_FOUND）。
        // 探测阶段读走的首字节作为前缀回填，保证完整内容（MODULE_NOT_FOUND 不会变成 ODULE_NOT_FOUND）。
        let stderr_tail = drain_stderr(stderr_handle, &probe[..probe_read]).await;

        let client = match serve_result {
            Some(Ok(client)) => client,
            Some(Err(e)) => {
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
            None => {
                // 超时：区分"进程无输出提前失败"与"总上限兜底"
                let actual = started.elapsed().as_secs();
                // 实际生效的上限（供错误消息展示）
                let effective_limit = if saw_stderr {
                    timeout_secs
                } else {
                    handshake_timeout_secs.min(timeout_secs)
                };
                let err = ConnectionError::Timeout {
                    elapsed_secs: actual,
                    timeout_secs: effective_limit,
                    stderr_tail: stderr_tail.clone(),
                };
                warn!(
                    "MCP server '{}' startup timed out after {}s (limit {}s, saw_stderr={}){}",
                    config.name,
                    actual,
                    effective_limit,
                    saw_stderr,
                    stderr_tail
                        .as_deref()
                        .map(|s| format!("\n  stderr: {}", s.replace('\n', "\n  ")))
                        .unwrap_or_default(),
                );
                *self.last_error.lock().await = Some(err);
                anyhow::bail!(
                    "MCP server '{}' startup timed out after {}s (limit {}s). \
                     If this is the first run (npx downloading packages), raise `timeout_secs`; \
                     if the process produced no output at all, raise `handshake_timeout_secs`.",
                    config.name, actual, effective_limit
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
        
        Ok(tools.iter().map(Self::convert_tool).collect())
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
