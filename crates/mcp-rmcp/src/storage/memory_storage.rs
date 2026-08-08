//! 内存存储实现：[`McpConfigStorage`] 的测试后端
//!
//! 基于 `RwLock<McpConfigFile>`，全部操作零 I/O，适合单元测试与未来 contract test。

use std::sync::RwLock;

use anyhow::Result;
use planned_agent_core::mcp::types::Tool;

use crate::config::{McpConfigFile, McpServerEntry, ToolEntry};
use crate::storage::trait_def::McpConfigStorage;

/// 基于内存的 [`McpConfigStorage`] 实现（测试场景）
pub struct InMemoryMcpConfigStorage {
    inner: RwLock<McpConfigFile>,
}

impl InMemoryMcpConfigStorage {
    /// 构造空配置
    pub fn new() -> Self {
        Self::with_config(McpConfigFile::default())
    }

    /// 构造预填充配置
    pub fn with_config(cfg: McpConfigFile) -> Self {
        Self {
            inner: RwLock::new(cfg),
        }
    }

    /// 当前快照（仅测试断言用）
    pub fn snapshot(&self) -> McpConfigFile {
        self.inner.read().unwrap().clone()
    }
}

impl Default for InMemoryMcpConfigStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl McpConfigStorage for InMemoryMcpConfigStorage {
    fn load_config(&self) -> Result<McpConfigFile> {
        Ok(self.inner.read().unwrap().clone())
    }

    fn save_config(&self, config: &McpConfigFile) -> Result<()> {
        *self.inner.write().unwrap() = config.clone();
        Ok(())
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
        } else {
            anyhow::bail!("MCP 服务器不存在: {}", server_name);
        }
        Ok(())
    }

    fn get_cached_tools(&self, server_name: &str) -> Vec<Tool> {
        self.load_config()
            .ok()
            .and_then(|cfg| cfg.servers.into_iter().find(|s| s.name == server_name))
            .map(|s| s.cached_tools.into_iter().map(|e| e.to_tool()).collect())
            .unwrap_or_default()
    }

    fn get_all_cached_tools(&self) -> Vec<(String, Vec<Tool>)> {
        self.load_config()
            .ok()
            .map(|cfg| {
                cfg.servers
                    .into_iter()
                    .map(|s| {
                        (
                            s.name,
                            s.cached_tools.into_iter().map(|e| e.to_tool()).collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(name: &str) -> McpServerEntry {
        McpServerEntry {
            name: name.into(),
            server_command: "echo".into(),
            server_args: vec![],
            transport: "stdio".into(),
            timeout_secs: None,
            max_retries: None,
            is_default: false,
            categories: None,
            tools_filter: None,
            cached_tools: vec![],
        }
    }

    fn tool(name: &str) -> Tool {
        Tool {
            name: name.into(),
            description: format!("desc of {}", name),
            input_schema: json!({}),
        }
    }

    #[test]
    fn crud_roundtrip() {
        // 使用空配置（不带 default "playwright" server）让断言精确
        let s = InMemoryMcpConfigStorage::with_config(McpConfigFile { servers: vec![] });

        // add: 空 → 1
        let cfg = s.add_server(entry("a")).unwrap();
        assert_eq!(cfg.servers.len(), 1);

        // update：仍为 1
        let updated = entry("a");
        s.update_server("a", updated).unwrap();
        assert_eq!(s.snapshot().servers.len(), 1);
        assert_eq!(s.snapshot().servers[0].name, "a");

        // cache_tools：仍为 1（工具列表是 server.cached_tools，与 servers.len() 不同）
        s.cache_tools("a", &[tool("t1"), tool("t2")]).unwrap();
        let cached = s.get_cached_tools("a");
        assert_eq!(cached.len(), 2);
        assert_eq!(cached[0].name, "t1");

        s.clear_cached_tools("a").unwrap();
        assert!(!s.has_cached_tools("a"));

        // delete: 1 → 0
        s.delete_server("a").unwrap();
        assert!(s.snapshot().servers.is_empty());

        // 不存在的 server：update / delete / cache_tools 全部报错
        assert!(s.update_server("ghost", entry("g")).is_err());
        assert!(s.delete_server("ghost").is_err());
        assert!(s.cache_tools("ghost", &[]).is_err());
    }
}