//! MCP 配置持久化管理
//!
//! 提供 MCP 服务器配置的 CRUD + 工具缓存（description / schema 保存到本地）。
//! 设计目标：
//! - 不连接 MCP 服务即可浏览已缓存的工具
//! - "刷新工具"时临时连接拉取，完成后断开
//! - 原子写入保证文件不损坏

use std::path::Path;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::info;

use planned_agent_core::types::{Tool, McpServerConfig};
use planned_agent_core::mcp::McpClient;

use crate::client::McpClientImpl;

// ═══════════════════════════════════════════════════════════════════════
// 数据结构
// ═══════════════════════════════════════════════════════════════════════

/// MCP 配置文件根结构（对应 mcp-config.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfigFile {
    #[serde(default)]
    pub servers: Vec<McpServerEntry>,
}

impl Default for McpConfigFile {
    fn default() -> Self {
        Self {
            servers: vec![McpServerEntry {
                name: "playwright".into(),
                server_command: "npx".into(),
                server_args: vec!["@playwright/mcp@latest".into()],
                transport: "stdio".into(),
                timeout_secs: Some(30),
                max_retries: Some(3),
                is_default: true,
                categories: Some(vec!["Browser".into()]),
                tools_filter: None,
                cached_tools: vec![],
            }],
        }
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
/// 负责配置文件读写、服务器 CRUD、工具缓存。
/// 不负责 MCP 连接管理——连接由 `McpManager` 负责。
#[derive(Clone)]
pub struct McpConfigManager {
    config_path: String,
}

impl McpConfigManager {
    /// 默认配置路径
    pub const DEFAULT_PATH: &'static str = "./data/mcp-config.json";

    pub fn new(config_path: &str) -> Self {
        Self {
            config_path: config_path.to_string(),
        }
    }

    // ── 文件读写 ────────────────────────────────────────────────────

    /// 加载配置；文件不存在时自动创建默认配置
    pub fn load_config(&self) -> Result<McpConfigFile> {
        if !Path::new(&self.config_path).exists() {
            if let Some(parent) = Path::new(&self.config_path).parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
            }
            let default_config = McpConfigFile::default();
            self.save_atomic(&default_config)?;
            info!("MCP 配置文件不存在，已创建默认配置: {}", self.config_path);
            return Ok(default_config);
        }

        let content = std::fs::read_to_string(&self.config_path)
            .with_context(|| format!("读取 MCP 配置文件失败: {}", self.config_path))?;
        let config: McpConfigFile = serde_json::from_str(&content)
            .with_context(|| format!("解析 MCP 配置文件失败: {}", self.config_path))?;
        Ok(config)
    }

    /// 保存配置（原子写入）
    pub fn save_config(&self, config: &McpConfigFile) -> Result<()> {
        self.save_atomic(config)
    }

    fn save_atomic(&self, config: &McpConfigFile) -> Result<()> {
        let tmp_path = format!("{}.tmp", self.config_path);
        let json = serde_json::to_string_pretty(config).context("序列化 MCP 配置失败")?;
        std::fs::write(&tmp_path, &json)
            .with_context(|| format!("写入临时文件失败: {}", tmp_path))?;
        std::fs::rename(&tmp_path, &self.config_path)
            .with_context(|| format!("原子重命名失败: {} -> {}", tmp_path, self.config_path))?;
        info!("MCP 配置已保存: {} servers", config.servers.len());
        Ok(())
    }

    // ── 服务器 CRUD ─────────────────────────────────────────────────

    pub fn add_server(&self, entry: McpServerEntry) -> Result<McpConfigFile> {
        let mut config = self.load_config()?;
        config.servers.push(entry);
        self.save_config(&config)?;
        Ok(config)
    }

    pub fn update_server(&self, name: &str, entry: McpServerEntry) -> Result<McpConfigFile> {
        let mut config = self.load_config()?;
        if let Some(existing) = config.servers.iter_mut().find(|s| s.name == name) {
            *existing = entry;
        } else {
            anyhow::bail!("MCP 服务器不存在: {}", name);
        }
        self.save_config(&config)?;
        Ok(config)
    }

    pub fn delete_server(&self, name: &str) -> Result<McpConfigFile> {
        let mut config = self.load_config()?;
        let original_len = config.servers.len();
        config.servers.retain(|s| s.name != name);
        if config.servers.len() == original_len {
            anyhow::bail!("MCP 服务器不存在: {}", name);
        }
        self.save_config(&config)?;
        Ok(config)
    }

    // ── 工具缓存 ────────────────────────────────────────────────────

    /// 将工具列表缓存到指定服务器的 `cached_tools` 字段
    pub fn cache_tools(&self, server_name: &str, tools: &[Tool]) -> Result<()> {
        let mut config = self.load_config()?;
        if let Some(server) = config.servers.iter_mut().find(|s| s.name == server_name) {
            server.cached_tools = tools.iter().map(ToolEntry::from_tool).collect();
            self.save_config(&config)?;
            info!("已缓存 {} 个工具 for server '{}'", tools.len(), server_name);
        } else {
            anyhow::bail!("MCP 服务器不存在: {}", server_name);
        }
        Ok(())
    }

    /// 读取缓存的工具（不连接 MCP）；无缓存时返回空 vec
    pub fn get_cached_tools(&self, server_name: &str) -> Vec<Tool> {
        match self.load_config() {
            Ok(config) => {
                config.servers.iter()
                    .find(|s| s.name == server_name)
                    .map(|s| s.cached_tools.iter().map(|e| e.to_tool()).collect())
                    .unwrap_or_default()
            }
            Err(_) => vec![],
        }
    }

    /// 读取所有服务器的缓存工具，返回 (server_name, tools)
    pub fn get_all_cached_tools(&self) -> Vec<(String, Vec<Tool>)> {
        match self.load_config() {
            Ok(config) => config.servers.iter()
                .map(|s| (s.name.clone(), s.cached_tools.iter().map(|e| e.to_tool()).collect()))
                .collect(),
            Err(_) => vec![],
        }
    }

    /// 清除指定服务器的缓存工具
    pub fn clear_cached_tools(&self, server_name: &str) -> Result<()> {
        let mut config = self.load_config()?;
        if let Some(server) = config.servers.iter_mut().find(|s| s.name == server_name) {
            server.cached_tools.clear();
            self.save_config(&config)?;
        }
        Ok(())
    }

    /// 检查指定服务器是否有缓存工具
    pub fn has_cached_tools(&self, server_name: &str) -> bool {
        !self.get_cached_tools(server_name).is_empty()
    }

    // ── 刷新工具 ────────────────────────────────────────────────────

    /// **刷新工具**：连接 MCP 服务器 → 拉取工具列表 → 缓存到文件 → 断开
    ///
    /// 这是 GUI "刷新工具"按钮对应的核心逻辑。
    /// 无论之前是否有缓存，都会重新连接拉取最新工具并覆盖缓存。
    pub async fn fetch_and_cache_tools(&self, server_entry: &McpServerEntry) -> Result<(String, Vec<Tool>)> {
        let server_name = server_entry.name.clone();
        let core_config: McpServerConfig = server_entry.into();

        info!("刷新工具: 连接 MCP server '{}'...", server_name);

        // 1. 临时连接
        let mut client = McpClientImpl::new();
        client.connect(core_config).await
            .with_context(|| format!("连接 MCP 服务器失败: {}", server_name))?;

        // 2. 拉取工具列表
        let tools = client.list_tools().await
            .with_context(|| format!("获取工具列表失败: {}", server_name))?;

        // 3. 应用过滤器（如果有）
        let filtered_tools = if let Some(filter) = &server_entry.tools_filter {
            tools.into_iter().filter(|t| filter.contains(&t.name)).collect()
        } else {
            tools
        };

        // 4. 缓存到文件
        self.cache_tools(&server_name, &filtered_tools)?;

        // 5. 断开
        client.disconnect().await?;

        info!("刷新完成: server '{}' 获取 {} 个工具", server_name, filtered_tools.len());
        Ok((server_name, filtered_tools))
    }
}
