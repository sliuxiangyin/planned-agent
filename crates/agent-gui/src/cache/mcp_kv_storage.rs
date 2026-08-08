//! GUI 侧的 KV 实现：把 MCP 配置存到 sled KV（与文本存储行为等价）
//!
//! ## KV 设计
//!
//! | key | value |
//! | --- | --- |
//! | `__servers__` | JSON 序列化的 `Vec<McpServerEntry>`（含 cached_tools） |
//!
//! **单 key 方案**与"每次 save 整个 `McpConfigFile`"的文本存储行为完全一致，
//! 避免维护 server 索引的复杂度。未来需要按 server 写入时可平滑迁移到：
//! `server:<name>` + `__index__`。
//!
//! ## 兼容性
//!
//! - 行为对齐 [`FileMcpConfigStorage`](planned_agent_mcp_rmcp::storage::FileMcpConfigStorage)：
//!   - 文件不存在时返回默认配置（**不**自动落盘，避免 KV 启动就污染）
//!   - CRUD 对不存在的 server 返回明确错误
//! - 写入后立即 `flush()`，保证崩溃后配置不丢（与原子写语义等价）

use std::sync::Arc;

use anyhow::{Context, Result};
use planned_agent_core::mcp::types::Tool;
use planned_agent_mcp_rmcp::config::{McpConfigFile, McpServerEntry, ToolEntry};
use planned_agent_mcp_rmcp::storage::McpConfigStorage;

use crate::cache::KvStore;

/// MCP 配置在 KV 中所属 tree 名
const MCP_CONFIG_TREE: &str = "mcp_config";

/// MCP 配置根 key：整个 `Vec<McpServerEntry>` 序列化后存这里
const SERVERS_KEY: &[u8] = b"__servers__";

/// 基于 sled KV 的 [`McpConfigStorage`] 实现（GUI 场景）
pub struct KvMcpConfigStorage {
    store: Arc<KvStore>,
}

impl KvMcpConfigStorage {
    /// 构造 KV 存储实现
    ///
    /// 调用方负责 [`KvStore`] 已初始化且 KV 进程唯一（sled 单进程锁）。
    pub fn new(store: Arc<KvStore>) -> Self {
        Self { store }
    }

    fn tree(&self) -> Result<sled::Tree> {
        self.store
            .open_tree(MCP_CONFIG_TREE)
            .context("打开 mcp_config tree 失败")
    }
}

impl McpConfigStorage for KvMcpConfigStorage {
    fn load_config(&self) -> Result<McpConfigFile> {
        let tree = self.tree()?;
        match self
            .store
            .get_json::<Vec<McpServerEntry>>(&tree, SERVERS_KEY)?
        {
            Some(servers) => Ok(McpConfigFile { servers }),
            // 第一次启动 KV 为空 → 返回默认配置（不自动持久化，留给首次写入触发）
            None => Ok(McpConfigFile::default()),
        }
    }

    fn save_config(&self, config: &McpConfigFile) -> Result<()> {
        let tree = self.tree()?;
        self.store
            .insert_json(&tree, SERVERS_KEY, &config.servers)
            .context("保存 MCP 配置到 KV 失败")?;
        // 显式 flush，保证 KV 行为与文件原子写等价
        self.store.flush().context("flush KV 失败")?;
        Ok(())
    }

    fn add_server(&self, entry: McpServerEntry) -> Result<McpConfigFile> {
        let mut cfg = self.load_config()?;
        cfg.servers.push(entry);
        self.save_config(&cfg)?;
        Ok(cfg)
    }

    fn update_server(&self, name: &str, entry: McpServerEntry) -> Result<McpConfigFile> {
        let mut cfg = self.load_config()?;
        if let Some(existing) = cfg.servers.iter_mut().find(|s| s.name == name) {
            *existing = entry;
        } else {
            anyhow::bail!("MCP 服务器不存在: {}", name);
        }
        self.save_config(&cfg)?;
        Ok(cfg)
    }

    fn delete_server(&self, name: &str) -> Result<McpConfigFile> {
        let mut cfg = self.load_config()?;
        let before = cfg.servers.len();
        cfg.servers.retain(|s| s.name != name);
        if cfg.servers.len() == before {
            anyhow::bail!("MCP 服务器不存在: {}", name);
        }
        self.save_config(&cfg)?;
        Ok(cfg)
    }

    fn cache_tools(&self, server_name: &str, tools: &[Tool]) -> Result<()> {
        let mut cfg = self.load_config()?;
        if let Some(server) = cfg.servers.iter_mut().find(|s| s.name == server_name) {
            server.cached_tools = tools.iter().map(ToolEntry::from_tool).collect();
            self.save_config(&cfg)?;
            tracing::info!(
                "已缓存 {} 个工具 for server '{}' (KV 后端)",
                tools.len(),
                server_name
            );
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
        let mut cfg = self.load_config()?;
        if let Some(server) = cfg.servers.iter_mut().find(|s| s.name == server_name) {
            server.cached_tools.clear();
            self.save_config(&cfg)?;
        }
        Ok(())
    }

    fn has_cached_tools(&self, server_name: &str) -> bool {
        !self.get_cached_tools(server_name).is_empty()
    }
}