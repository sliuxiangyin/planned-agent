# MCP 集成 (`crates/mcp-rmcp`)

实现 `McpClient` trait，封装 rmcp 库。

## 目录结构

```
crates/mcp-rmcp/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── client.rs      # MCP 客户端实现
    ├── manager.rs     # MCP 管理器（多服务器支持）
    └── tools.rs       # 工具管理（多服务器支持）
```

## 关键功能

- 支持多种传输层：stdio、TCP、WebSocket
- 工具发现：自动列出 MCP 服务器提供的工具
- 工具调用：执行工具并返回结果
- 工具定义转换：将 MCP 工具转换为 OpenAI 函数调用格式

## 与 AI 集成

```rust
// 将 MCP 工具转换为 OpenAI 函数定义
fn convert_tools_to_openai(tools: &[Tool]) -> Vec<openai::Tool> {
    tools.iter().map(|tool| {
        openai::Tool {
            r#type: "function".to_string(),
            function: FunctionDescription {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            },
        }
    }).collect()
}
```

## MCP 连接管理和错误处理

### 问题分析

MCP连接可能出现以下情况导致工具调用失败：
1. **网络问题**：TCP/WebSocket连接可能因网络波动断开
2. **服务器崩溃**：MCP服务器进程意外终止
3. **连接超时**：长时间空闲导致连接超时关闭
4. **资源耗尽**：服务器内存或连接数达到上限
5. **主动断开**：服务器主动关闭连接（如维护、重启）

### 解决方案

#### 1. 连接健康检查

在 `McpClient` trait 中增加健康检查方法：

```rust
#[async_trait]
pub trait McpClient: Send + Sync {
    // ... 其他方法 ...
    
    /// 检查连接是否健康
    async fn is_connected(&self) -> bool;
    
    /// 获取连接状态信息
    async fn connection_status(&self) -> ConnectionStatus;
    
    /// 心跳检测（如果服务器支持）
    async fn ping(&self) -> Result<()>;
}
```

#### 2. 自动重连机制

实现带重试的连接管理：

```rust
pub struct ResilientMcpClient {
    inner: Box<dyn McpClient>,
    config: McpConfig,
    retry_policy: RetryPolicy,
}

impl ResilientMcpClient {
    /// 执行带重试的操作
    async fn execute_with_retry<F, T>(&self, operation: F) -> Result<T>
    where
        F: Fn() -> Pin<Box<dyn Future<Output = Result<T>> + Send>>,
    {
        let mut attempts = 0;
        loop {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) if self.is_retriable_error(&e) && attempts < self.retry_policy.max_attempts => {
                    attempts += 1;
                    tracing::warn!("MCP operation failed (attempt {}): {}", attempts, e);
                    
                    // 如果连接断开，尝试重连
                    if self.is_connection_error(&e) {
                        self.reconnect().await?;
                    }
                    
                    // 指数退避
                    let delay = self.retry_policy.delay(attempts);
                    tokio::time::sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }
    
    /// 重新连接
    async fn reconnect(&mut self) -> Result<()> {
        tracing::info!("Attempting to reconnect to MCP server...");
        self.inner.disconnect().await.ok(); // 忽略断开错误
        self.inner.connect(self.config.clone()).await
    }
}
```

#### 3. 连接池管理

对于高并发场景，可以考虑连接池：

```rust
use tokio::sync::Pool;

pub struct McpConnectionPool {
    pool: Pool<ResilientMcpClient>,
    config: McpConfig,
}

impl McpConnectionPool {
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult> {
        let client = self.pool.get().await?;
        client.call_tool(name, arguments).await
    }
}
```

#### 4. 断路器模式

防止持续调用失败的服务器：

```rust
pub struct CircuitBreaker {
    failure_count: AtomicU32,
    threshold: u32,
    reset_timeout: Duration,
    last_failure: Mutex<Option<Instant>>,
}

impl CircuitBreaker {
    pub async fn call<F, T>(&self, operation: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        if self.is_open() {
            return Err(anyhow::anyhow!("Circuit breaker is open"));
        }
        
        match operation.await {
            Ok(result) => {
                self.record_success();
                Ok(result)
            }
            Err(e) => {
                self.record_failure();
                Err(e)
            }
        }
    }
}
```

## 在主程序中的集成

```rust
// 在 agent.rs 中
pub struct Agent {
    ai_client: Box<dyn AiClient>,
    mcp_client: ResilientMcpClient,
    tools: Vec<Tool>,
}

impl Agent {
    pub async fn process_input(&mut self, input: &str) -> Result<String> {
        // 1. 确保 MCP 连接健康
        if !self.mcp_client.is_connected().await {
            tracing::info!("MCP connection lost, reconnecting...");
            self.mcp_client.reconnect().await?;
            self.refresh_tools().await?;
        }
        
        // 2. 获取工具定义
        let tools = self.get_tools().await?;
        
        // 3. 发送给 AI（带工具定义）
        let response = self.ai_client.complete(
            AiRequest::new(input).with_tools(tools)
        ).await?;
        
        // 4. 处理工具调用
        if let Some(tool_calls) = response.tool_calls() {
            for call in tool_calls {
                let result = self.mcp_client.call_tool(
                    &call.name,
                    call.arguments.clone()
                ).await?;
                
                // 将工具结果反馈给 AI
                // ...
            }
        }
        
        Ok(response.content())
    }
    
    async fn refresh_tools(&mut self) -> Result<()> {
        self.tools = self.mcp_client.list_tools().await?;
        Ok(())
    }
}
```

## 监控和指标

添加连接状态监控：

```rust
pub struct McpMetrics {
    total_calls: AtomicU64,
    failed_calls: AtomicU64,
    reconnections: AtomicU64,
    average_latency: Mutex<Duration>,
}

impl McpMetrics {
    pub fn record_call(&self, success: bool, latency: Duration) {
        self.total_calls.fetch_add(1, Ordering::Relaxed);
        if !success {
            self.failed_calls.fetch_add(1, Ordering::Relaxed);
        }
        // 更新平均延迟
    }
}
```

## 配置选项

在配置文件中增加连接管理选项：

```toml
[mcp]
server_command = "npx"
server_args = ["-y", "@modelcontextprotocol/server-everything"]
transport = "stdio"

[mcp.connection]
max_retries = 3
retry_delay_ms = 1000
timeout_secs = 30
heartbeat_interval_secs = 60
max_pool_size = 5
```