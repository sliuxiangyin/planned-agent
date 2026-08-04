//! MCP 配置管理器 —— 存储后端可插拔
//!
//! ## 设计
//!
//! `McpConfigManager` **不直接做文件 I/O**，全部 CRUD / 缓存操作委托给
//! [`crate::storage::McpConfigStorage`] trait 实现。
//! 调用方按场景选择后端：
//!
//! - **CLI 场景**：使用 [`crate::storage::FileMcpConfigStorage`]（默认）
//! - **GUI 场景**：调用方注入自实现的 KV / DB 存储
//! - **测试场景**：使用 [`crate::storage::InMemoryMcpConfigStorage`]
//!
//! ## 连接 vs 配置
//!
//! 本管理器**只管配置持久化**，不负责 MCP 连接管理——
//! 连接由 [`crate::manager::McpManager`] 负责。

use std::fmt;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;

use planned_agent_core::types::{Tool, McpServerConfig, ConnectionError};
use planned_agent_core::mcp::McpClient;

use crate::client::McpClientImpl;
use crate::storage::McpConfigStorage;

/// 刷新工具失败时携带的结构化信息
///
/// `source` 是原始 anyhow 错误（含完整上下文链），
/// `connection_error` 是 MCP 连接失败阶段的结构化分类（供 UI 分类展示）。
#[derive(Debug)]
pub struct McpRefreshError {
    pub source: anyhow::Error,
    pub connection_error: Option<ConnectionError>,
}

impl fmt::Display for McpRefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl std::error::Error for McpRefreshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 数据结构
// ═══════════════════════════════════════════════════════════════════════

/// MCP 配置文件根结构（对应 mcp-config.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfigFile {
    #[serde(default)]
    pub servers: Vec<McpServerEntry>,
}

/// `McpConfigFile` 默认值：空服务器列表
///
/// **重要变更**（KV 抽象化后）：原本默认含一个 `playwright` server，
/// 但那会导致 KV 首次启动时 "幽灵出现" 一个未被持久化的 server，
/// 给用户造成 "删不掉、KV 一旦清空又复活" 的错觉。
///
/// 当前策略：default 始终为空。CLI / GUI 首次启动列表为空，
/// 用户通过设置页手动添加 server（更符合 "用户控制" 原则）。
impl Default for McpConfigFile {
    fn default() -> Self {
        Self { servers: vec![] }
    }
}

/// 单个 MCP 服务器配置项（持久化版本，含缓存工具）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    pub server_command: String,
    #[serde(default)]
    pub server_args: Vec<String>,
    #[serde(default = "default_transport")]
    pub transport: String,
    /// 连接超时上限（秒）。覆盖完整冷启动链：
    /// spawn 子进程 → npx 拉包（首次可达数十秒）→ MCP initialize 握手。
    /// None 时使用 client 的默认 120s。
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub categories: Option<Vec<String>>,
    #[serde(default)]
    pub tools_filter: Option<Vec<String>>,
    /// 缓存的工具列表（name + description + input_schema）
    #[serde(default)]
    pub cached_tools: Vec<ToolEntry>,
}

/// 缓存的单个工具条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolEntry {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

fn default_transport() -> String {
    "stdio".to_string()
}

// ═══════════════════════════════════════════════════════════════════════
// 转换
// ═══════════════════════════════════════════════════════════════════════

impl McpServerEntry {
    /// 转为 core 层的 McpServerConfig（运行时用）
    pub fn to_core_config(&self) -> McpServerConfig {
        McpServerConfig {
            name: self.name.clone(),
            server_command: self.server_command.clone(),
            server_args: self.server_args.clone(),
            transport: self.transport.clone(),
            timeout_secs: self.timeout_secs,
            max_retries: self.max_retries,
            is_default: self.is_default,
            tools_filter: self.tools_filter.clone(),
            categories: self.categories.clone(),
        }
    }
}

impl ToolEntry {
    pub fn from_tool(tool: &Tool) -> Self {
        Self {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
        }
    }

    pub fn to_tool(&self) -> Tool {
        Tool {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }
}

impl From<&McpServerEntry> for McpServerConfig {
    fn from(entry: &McpServerEntry) -> Self {
        entry.to_core_config()
    }
}

// ═══════════════════════════════════════════════════════════════════════
// McpConfigManager
// ═══════════════════════════════════════════════════════════════════════

/// MCP 配置管理器
///
/// **存储后端可插拔**：通过 [`Arc<dyn McpConfigStorage>`] 注入。
/// CLI 用 [`crate::storage::FileMcpConfigStorage`]，GUI / 测试可注入其他实现。
///
/// 负责配置文件读写、服务器 CRUD、工具缓存；
/// 不负责 MCP 连接管理——连接由 [`crate::manager::McpManager`] 负责。
#[derive(Clone)]
pub struct McpConfigManager {
    storage: Arc<dyn McpConfigStorage>,
}

impl McpConfigManager {
    /// 默认配置文件路径（CLI 场景）
    pub const DEFAULT_PATH: &'static str = "./data/mcp-config.json";

    /// **便捷构造**（向后兼容）：直接传路径，内部用 [`crate::storage::FileMcpConfigStorage`]
    ///
    /// CLI 调用方继续用 `McpConfigManager::new(DEFAULT_PATH)` 即可。
    pub fn new(config_path: &str) -> Self {
        Self::with_storage(Arc::new(
            crate::storage::FileMcpConfigStorage::new(config_path),
        ))
    }

    /// **DI 构造**：调用方注入任意 [`McpConfigStorage`] 实现
    ///
    /// GUI 场景传 `KvMcpConfigStorage`；测试场景传 `InMemoryMcpConfigStorage`。
    pub fn with_storage(storage: Arc<dyn McpConfigStorage>) -> Self {
        Self { storage }
    }

    /// 访问内部 storage（高级用法：直接读写底层后端）
    pub fn storage(&self) -> &Arc<dyn McpConfigStorage> {
        &self.storage
    }

    // ── 全部方法委托给 storage ──

    /// 加载配置；底层不存在时自动创建默认配置
    pub fn load_config(&self) -> Result<McpConfigFile> {
        self.storage.load_config()
    }

    /// 保存配置（原子 / 事务语义由 storage 实现保证）
    pub fn save_config(&self, config: &McpConfigFile) -> Result<()> {
        self.storage.save_config(config)
    }

    // ── 服务器 CRUD ──

    pub fn add_server(&self, entry: McpServerEntry) -> Result<McpConfigFile> {
        self.storage.add_server(entry)
    }

    pub fn update_server(&self, name: &str, entry: McpServerEntry) -> Result<McpConfigFile> {
        self.storage.update_server(name, entry)
    }

    pub fn delete_server(&self, name: &str) -> Result<McpConfigFile> {
        self.storage.delete_server(name)
    }

    // ── 工具缓存 ──

    /// 将工具列表缓存到指定服务器的 `cached_tools` 字段
    pub fn cache_tools(&self, server_name: &str, tools: &[Tool]) -> Result<()> {
        self.storage.cache_tools(server_name, tools)
    }

    /// 读取缓存的工具（不连接 MCP）；无缓存时返回空 vec
    pub fn get_cached_tools(&self, server_name: &str) -> Vec<Tool> {
        self.storage.get_cached_tools(server_name)
    }

    /// 读取所有服务器的缓存工具，返回 (server_name, tools)
    pub fn get_all_cached_tools(&self) -> Vec<(String, Vec<Tool>)> {
        self.storage.get_all_cached_tools()
    }

    /// 清除指定服务器的缓存工具
    pub fn clear_cached_tools(&self, server_name: &str) -> Result<()> {
        self.storage.clear_cached_tools(server_name)
    }

    /// 检查指定服务器是否有缓存工具
    pub fn has_cached_tools(&self, server_name: &str) -> bool {
        self.storage.has_cached_tools(server_name)
    }

    // ── 刷新工具（复合操作，保留在 manager） ──

    /// **刷新工具**：连接 MCP 服务器 → 拉取工具列表 → 缓存到 storage → 断开
    ///
    /// 这是 GUI "刷新工具"按钮对应的核心逻辑。
    /// 无论之前是否有缓存，都会重新连接拉取最新工具并覆盖缓存。
    ///
    /// ## 错误
    /// 失败时返回 `McpRefreshError`，其中 `connection_error` 携带结构化错误分类
    /// （Timeout / Spawn / Handshake），由 UI 层用于差异化展示。
    pub async fn fetch_and_cache_tools(
        &self,
        server_entry: &McpServerEntry,
    ) -> std::result::Result<(String, Vec<Tool>), McpRefreshError> {
        let server_name = server_entry.name.clone();
        let core_config: McpServerConfig = server_entry.into();

        info!("刷新工具: 连接 MCP server '{}'...", server_name);

        // 1. 临时连接（失败时抓取结构化错误）
        let mut client = McpClientImpl::new();
        if let Err(e) = client.connect(core_config).await {
            let conn_err = client.take_last_error().await;
            return Err(McpRefreshError {
                source: e.context(format!("连接 MCP 服务器失败: {}", server_name)),
                connection_error: conn_err,
            });
        }

        // 2. 拉取工具列表
        let tools = client
            .list_tools()
            .await
            .map_err(|e| McpRefreshError {
                source: e.context(format!("获取工具列表失败: {}", server_name)),
                connection_error: None,
            })?;

        // 3. 应用过滤器（如果有）
        let filtered_tools = if let Some(filter) = &server_entry.tools_filter {
            tools.into_iter().filter(|t| filter.contains(&t.name)).collect()
        } else {
            tools
        };

        // 4. 缓存到 storage（接口反转点）
        self.cache_tools(&server_name, &filtered_tools).map_err(|e| McpRefreshError {
            source: e.context(format!("缓存工具列表失败: {}", server_name)),
            connection_error: None,
        })?;

        // 5. 断开
        client.disconnect().await.map_err(|e| McpRefreshError {
            source: e.context(format!("断开 MCP 连接失败: {}", server_name)),
            connection_error: None,
        })?;

        info!(
            "刷新完成: server '{}' 获取 {} 个工具",
            server_name,
            filtered_tools.len()
        );
        Ok((server_name, filtered_tools))
    }
}