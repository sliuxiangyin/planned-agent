# MCP 集成 (`crates/mcp-rmcp`)

实现 `McpClient` trait，封装 rmcp 库。

> 下文含部分早期设计构想。**现行真实架构见本节**。

## 实际架构（现行 · `McpManager` 单门面）

对外只暴露一个门面 `McpManager`，同时承担两类职责：

- **持久化**：server 配置 / 工具缓存 / 连接状态（内部聚合 config + status 两个 storage，后端可插拔：GUI 用 KV、CLI 用 File）
- **运行时**：连接 / 断开 / 懒连接 / 工具调用与路由（`tool→server` 映射）

公开类型已收口：`McpManager` + 数据模型（`McpServerView` / `McpServerEntry` / `McpConfigFile` / `ServerStatus` / `Tool` …）+ `storage` traits/实现 + `McpClientImpl`。`McpConfigManager` / `McpBundle` 已降为 crate 内部实现（`pub(crate)`），**不再导出**。

### 门面方法分组（见 `docs/mcp-unify-refactor.md` A.0）

| 组 | 方法 |
|---|---|
| 构造 | `new()`(File 默认) / `with_backends(config_storage, status_storage)` |
| 服务 CRUD | `list_servers()` / `get_server()` / `load_config()` / `add_server` / `update_server` / `delete_server` |
| 刷新 / 预载 | `preload_cached_tools()`(缓存→路由表，不连接) / `refresh_server_tools()`(连→拉→缓存→路由+自动记状态) |
| 状态 | `record_status` / `get_status` / `list_status` / `delete_status` / `has_status` / `record_failure` |
| 连接 | `connect_server/all` / `disconnect_server/all` / `is_server_connected` / `call_tool` / `call_tool_auto`(懒连) |
| 工具 | `get_server_tools`(登记表) / `get_all_tools` / `server_tools`(读登记表，不触发连接) |

### 读路径 / 写路径

- 读路径（`list_servers` / `get_server` / `server_tools` / 状态）**无副作用、不触发连接**。
- 连接/拉取只发生在写路径：`refresh_server_tools` / 显式 `connect_server` / `call_tool_auto` 懒连。

### 冷启动与装配（GUI）

1. `McpManager::with_backends(kv)` 构造 → `preload_cached_tools()` 把缓存工具喂进运行时路由表（不连任何 server）。
2. `ToolRegistry::set_mcp_manager(manager)` 从 manager 拉取并**统一注册** MCP 工具（唯一入口，避免重复注册）。
3. 首次调用某 server 工具 → `call_tool_auto` 懒连接。

### 运行期刷新单 server（GUI）

`manager.refresh_server_tools(name)`（拉新工具并更新路由+状态）→ `registry.sync_mcp_server(name)`（卸旧+按登记表重注册；分类映射统一在 tool-manager 的 `map_categories`）。

## 目录结构

```
crates/mcp-rmcp/src/
├── lib.rs            # 对外导出：McpManager + 数据模型 + storage
├── client.rs         # McpClientImpl（单 server 真实连接：spawn/握手/list_tools/call_tool）
├── command_resolver.rs
├── tools.rs          # ToolManager（server→tools 运行时路由表，内部）
├── manager/
│   ├── mod.rs        # struct + 内部状态 + 构造(new/with_backends)
│   ├── routing.rs    # 运行时：连接/断开/懒连/工具注入/调用/查询 + McpManagerTrait
│   ├── config.rs     # ① 服务 CRUD（持久化 config/tools cache）
│   ├── status.rs     # ③ 连接状态读写
│   ├── views.rs      # load_servers / get_server（config+status join 视图）
│   └── refresh.rs    # 预载缓存工具 / 刷新某 server 工具
├── bundle.rs         # McpBundle（内部实现，被 manager 聚合）
├── config.rs         # McpConfigManager + 数据模型（内部）
└── storage/          # McpConfigStorage / McpStatusStorage traits + File/InMemory
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