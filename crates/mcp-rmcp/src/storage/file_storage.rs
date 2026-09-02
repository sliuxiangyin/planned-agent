//! 文件存储实现：[`McpConfigStorage`] 的默认后端
//!
//! 行为完全对齐重构前的 `crate::config::McpConfigManager`：
//! - JSON 格式 + 原子写入（tmp + rename）
//! - 文件不存在时自动创建默认配置
//! - 全部 server CRUD 与工具缓存 API 1:1 保留

use std::path::Path;

use anyhow::{Context, Result};
use planned_agent_core::mcp::types::Tool;
use tracing::info;

use crate::config::{McpConfigFile, McpServerEntry, ToolEntry};
use crate::storage::trait_def::McpConfigStorage;

/// 基于 JSON 文件的 [`McpConfigStorage`] 实现（CLI 场景默认）
pub struct FileMcpConfigStorage {
    config_path: String,
}

impl FileMcpConfigStorage {
    /// 构造文件存储；调用方负责保证 `path` 的父目录可写
    pub fn new(config_path: &str) -> Self {
        Self {
            config_path: config_path.to_string(),
        }
    }

    /// 默认配置文件路径（CLI 场景）
    pub fn default_path() -> &'static str {
        crate::config::McpConfigManager::DEFAULT_PATH
    }

    /// 原子写入：写到 `<path>.tmp` 后 `rename` 到目标
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
}

impl McpConfigStorage for FileMcpConfigStorage {
    fn load_config(&self) -> Result<McpConfigFile> {
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

    fn save_config(&self, config: &McpConfigFile) -> Result<()> {
        self.save_atomic(config)
    }

    fn add_server(&self, entry: McpServerEntry) -> Result<McpConfigFile> {
        let mut config = self.load_config()?;
        config.servers.push(entry);
        self.save_config(&config)?;
        Ok(config)
    }

    fn update_server(&self, name: &str, entry: McpServerEntry) -> Result<McpConfigFile> {
        let mut config = self.load_config()?;
        if let Some(existing) = config.servers.iter_mut().find(|s| s.name == name) {
            *existing = entry;
        } else {
            anyhow::bail!("MCP 服务器不存在: {}", name);
        }
        self.save_config(&config)?;
        Ok(config)
    }

    fn delete_server(&self, name: &str) -> Result<McpConfigFile> {
        let mut config = self.load_config()?;
        let original_len = config.servers.len();
        config.servers.retain(|s| s.name != name);
        if config.servers.len() == original_len {
            anyhow::bail!("MCP 服务器不存在: {}", name);
        }
        self.save_config(&config)?;
        Ok(config)
    }

    fn cache_tools(&self, server_name: &str, tools: &[Tool]) -> Result<()> {
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

    fn get_cached_tools(&self, server_name: &str) -> Vec<Tool> {
        match self.load_config() {
            Ok(config) => config
                .servers
                .iter()
                .find(|s| s.name == server_name)
                .map(|s| s.cached_tools.iter().map(|e| e.to_tool()).collect())
                .unwrap_or_default(),
            Err(_) => vec![],
        }
    }

    fn get_all_cached_tools(&self) -> Vec<(String, Vec<Tool>)> {
        match self.load_config() {
            Ok(config) => config
                .servers
                .iter()
                .map(|s| {
                    (
                        s.name.clone(),
                        s.cached_tools.iter().map(|e| e.to_tool()).collect(),
                    )
                })
                .collect(),
            Err(_) => vec![],
        }
    }

    fn clear_cached_tools(&self, server_name: &str) -> Result<()> {
        let mut config = self.load_config()?;
        if let Some(server) = config.servers.iter_mut().find(|s| s.name == server_name) {
            server.cached_tools.clear();
            self.save_config(&config)?;
        }
        Ok(())
    }

    fn has_cached_tools(&self, server_name: &str) -> bool {
        !self.get_cached_tools(server_name).is_empty()
    }
}