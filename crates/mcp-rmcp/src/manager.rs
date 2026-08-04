use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use anyhow::Result;
use planned_agent_core::mcp::McpClient;
use planned_agent_core::types::{Tool, ToolResult, McpServerConfig};
use serde_json::Value;
use tracing::{info, error};
use async_trait::async_trait;
use crate::client::McpClientImpl;
use crate::tools::ToolManager;
// McpManagerTrait 已下沉到 core，避免 mcp-rmcp 反向依赖 tool-manager
use planned_agent_core::tool_registry::traits::McpManagerTrait;

// ═══════════════════════════════════════════════════════════════════════
// 内部可变状态（std::sync::RwLock 支持多读单写）
// ═══════════════════════════════════════════════════════════════════════

struct McpManagerInner {
    /// 已连接的客户端（Arc 允许跨 await 持有引用）
    clients: HashMap<String, Arc<McpClientImpl>>,
    /// 内部工具映射（server → tools），用于 call_tool_auto 路由
    tool_manager: ToolManager,
    /// 服务器配置缓存（用于懒连接）
    server_configs: HashMap<String, McpServerConfig>,
}

impl McpManagerInner {
    fn new() -> Self {
        Self {
            clients: HashMap::new(),
            tool_manager: ToolManager::new(),
            server_configs: HashMap::new(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// McpManager（线程安全 + 懒连接）
// ═══════════════════════════════════════════════════════════════════════

/// MCP 管理器
///
/// 使用 `Arc<RwLock<McpManagerInner>>` 实现内部可变性：
/// - `connect_server` / `disconnect_server` 获取 write lock
/// - `call_tool` 获取 read lock 后 clone Arc，释放锁再 await
/// - 支持懒连接：`call_tool_auto` 自动连接未连接的 server
pub struct McpManager {
    inner: Arc<RwLock<McpManagerInner>>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(McpManagerInner::new())),
        }
    }

    // ── 连接管理 ─────────────────────────────────────────────────────

    /// 完整连接：connect → list_tools → 注册内部 ToolManager
    pub async fn connect_server(&self, config: McpServerConfig) -> Result<()> {
        let name = config.name.clone();
        info!("Connecting to MCP server: {}", name);

        let mut client = McpClientImpl::new();
        client.connect(config.clone()).await?;
        let tools = client.list_tools().await?;

        let filtered = if let Some(filter) = &config.tools_filter {
            tools.into_iter().filter(|t| filter.contains(&t.name)).collect()
        } else {
            tools
        };

        let mut inner = self.inner.write().unwrap();
        inner.tool_manager.add_tools(&name, filtered);
        inner.clients.insert(name.clone(), Arc::new(client));
        inner.server_configs.insert(name.clone(), config);

        let count = inner.tool_manager.get_server_tools(&name).len();
        info!("Connected to MCP server: {} with {} tools", name, count);
        Ok(())
    }

    /// 懒连接：仅建传输，不调 list_tools()（工具已在 ToolManager 中）
    async fn connect_server_lazy(&self, config: &McpServerConfig) -> Result<()> {
        let name = config.name.clone();
        info!("Lazy connecting to MCP server: {}", name);

        let mut client = McpClientImpl::new();
        client.connect(config.clone()).await?;

        let mut inner = self.inner.write().unwrap();
        inner.clients.insert(name.clone(), Arc::new(client));
        inner.server_configs.insert(name.clone(), config.clone());

        info!("Lazy connected to MCP server: {}", name);
        Ok(())
    }

    /// 批量连接
    pub async fn connect_all(&self, configs: Vec<McpServerConfig>) -> Result<()> {
        for config in configs {
            if let Err(e) = self.connect_server(config).await {
                error!("Failed to connect to MCP server: {}", e);
            }
        }
        Ok(())
    }

    /// 断开单个服务器
    pub async fn disconnect_server(&self, name: &str) -> Result<()> {
        let removed = {
            let mut inner = self.inner.write().unwrap();
            inner.tool_manager.remove_server_tools(name);
            inner.server_configs.remove(name);
            inner.clients.remove(name)
        };

        if let Some(client_arc) = removed {
            // 尝试取出所有权以调用 disconnect(&mut self)
            if let Ok(mut client) = Arc::try_unwrap(client_arc) {
                client.disconnect().await?;
            }
            // 如果 Arc 还有其他引用，drop 时会自动清理
            info!("Disconnected from MCP server: {}", name);
        }
        Ok(())
    }

    /// 断开所有
    pub async fn disconnect_all(&self) -> Result<()> {
        let names: Vec<String> = {
            let inner = self.inner.read().unwrap();
            inner.clients.keys().cloned().collect()
        };
        for name in names {
            self.disconnect_server(&name).await?;
        }
        Ok(())
    }

    // ── 工具注入（外部缓存 → 内部 ToolManager，不走 list_tools） ──

    /// 将工具注入内部 ToolManager（不连接服务器）
    ///
    /// 与 `ToolRegistry` 注册配合使用：外部（agent-gui）从缓存注册工具到
    /// ToolRegistry 后，调用此方法同步 McpManager 的内部路由表，
    /// 确保后续 `call_tool_auto` 能找到工具所属服务器并懒连接。
    pub fn set_server_tools(&self, server_name: &str, tools: Vec<Tool>) {
        let mut inner = self.inner.write().unwrap();
        inner.tool_manager.add_tools(server_name, tools);
        info!(
            "McpManager: synced {} tools for server '{}'",
            inner.tool_manager.get_server_tools(server_name).len(),
            server_name
        );
    }

    /// 注入工具 + 服务器配置（首次从缓存加载时使用）
    pub fn set_server_tools_with_config(
        &self,
        server_name: &str,
        tools: Vec<Tool>,
        config: McpServerConfig,
    ) {
        let mut inner = self.inner.write().unwrap();
        inner.tool_manager.add_tools(server_name, tools);
        inner.server_configs.insert(server_name.to_string(), config);
        info!(
            "McpManager: synced {} tools + config for server '{}'",
            inner.tool_manager.get_server_tools(server_name).len(),
            server_name
        );
    }

    // ── 工具调用 ─────────────────────────────────────────────────────

    /// 调用指定服务器的工具
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ToolResult> {
        // read lock → clone Arc → release lock → await
        let client = {
            let inner = self.inner.read().unwrap();
            inner.clients.get(server_name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("MCP server not connected: {}", server_name))?
        };

        info!("Calling tool '{}' on server '{}'", tool_name, server_name);
        client.call_tool(tool_name, arguments).await
    }

    /// 自动路由：按工具名找 server → 懒连接（如需）→ 调用
    pub async fn call_tool_auto(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        // 1. 查找 server
        let server_name = {
            let inner = self.inner.read().unwrap();
            inner.tool_manager.find_server_for_tool(tool_name)
                .ok_or_else(|| anyhow::anyhow!("No server found for tool: {}", tool_name))?
        };

        // 2. 懒连接（如未连接）
        let need_connect = {
            let inner = self.inner.read().unwrap();
            !inner.clients.contains_key(&server_name)
        };
        if need_connect {
            let config = {
                let inner = self.inner.read().unwrap();
                inner.server_configs.get(&server_name).cloned()
            };
            if let Some(ref cfg) = config {
                self.connect_server_lazy(cfg).await?;
            } else {
                return Err(anyhow::anyhow!(
                    "No config for MCP server '{}' — cannot lazy connect",
                    server_name
                ));
            }
        }

        // 3. 调用
        self.call_tool(&server_name, tool_name, arguments).await
    }

    // ── 查询 ─────────────────────────────────────────────────────────

    pub fn get_all_tools(&self) -> Vec<Tool> {
        self.inner.read().unwrap().tool_manager.get_all_tools()
    }

    pub fn get_server_tools(&self, server_name: &str) -> Vec<Tool> {
        self.inner.read().unwrap().tool_manager.get_server_tools(server_name)
    }

    pub fn get_server_names(&self) -> Vec<String> {
        self.inner.read().unwrap().clients.keys().cloned().collect()
    }

    pub fn is_server_connected(&self, server_name: &str) -> bool {
        self.inner.read().unwrap().clients.contains_key(server_name)
    }
}

// ═══════════════════════════════════════════════════════════════════════
// McpManagerTrait 实现
// ═══════════════════════════════════════════════════════════════════════

#[async_trait]
impl McpManagerTrait for McpManager {
    async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        self.call_tool_auto(tool_name, arguments).await
    }

    fn get_all_tools(&self) -> Vec<Tool> {
        self.get_all_tools()
    }

    fn find_server_for_tool(&self, tool_name: &str) -> Option<String> {
        self.inner.read().unwrap().tool_manager.find_server_for_tool(tool_name)
    }

    fn get_server_names(&self) -> Vec<String> {
        self.get_server_names()
    }

    fn get_server_categories(&self, server_name: &str) -> Option<Vec<String>> {
        let inner = self.inner.read().unwrap();
        inner.server_configs.get(server_name).and_then(|c| c.categories.clone())
    }
}
