//! GUI 侧的 KV 实现：把 MCP 连接状态存到 sled KV
//!
//! ## KV 设计
//!
//! 与 [`KvMcpConfigStorage`] **不同**——状态用 **per-server key**，不存单 blob：
//!
//! | key | value |
//! | --- | --- |
//! | `server:<name>` | JSON 序列化的 [`ServerStatus`] |
//!
//! ## 为什么是 per-server key（而不是单 blob）
//!
//! - 状态更新**频繁**（每次 connect 尝试写一次），单 blob 方式会重写整张表
//! - `KvMcpConfigStorage` 用单 blob 是因为 config 写入**低频**且需要整表事务；
//!   状态无此约束
//! - per-server key 让 `delete` 操作天然原子，无需 load-modify-save
//! - sled 的树内迭代（用于 `load_all`）O(n)，n = server 数量，足够小
//!
//! ## 兼容性
//!
//! - 行为对齐 [`FileMcpStatusStorage`](planned_agent_mcp_rmcp::storage::FileMcpStatusStorage)：
//!   - 底层不存在时返回空 map（**不**自动落盘）
//!   - `delete` 不存在的 key 是 no-op
//! - 写入后立即 `flush()`，保证崩溃后状态不丢（与 KV 配置行为对齐）

use std::sync::Arc;

use anyhow::{Context, Result};
use planned_agent_mcp_rmcp::storage::{McpStatusStorage, ServerStatus};

use crate::cache::KvStore;

/// MCP 连接状态在 KV 中所属 tree 名（与 `mcp_config` 隔离）
const MCP_STATUS_TREE: &str = "mcp_status";

/// 状态 key 前缀：完整 key = `server:<name>`
const SERVER_KEY_PREFIX: &str = "server:";

fn status_key(name: &str) -> Vec<u8> {
    format!("{}{}", SERVER_KEY_PREFIX, name).into_bytes()
}

/// 基于 sled KV 的 [`McpStatusStorage`] 实现（GUI 场景）
pub struct KvMcpStatusStorage {
    store: Arc<KvStore>,
}

impl KvMcpStatusStorage {
    /// 构造 KV 存储实现
    ///
    /// 调用方负责 [`KvStore`] 已初始化且 KV 进程唯一（sled 单进程锁）。
    pub fn new(store: Arc<KvStore>) -> Self {
        Self { store }
    }

    fn tree(&self) -> Result<sled::Tree> {
        self.store
            .open_tree(MCP_STATUS_TREE)
            .context("打开 mcp_status tree 失败")
    }
}

impl McpStatusStorage for KvMcpStatusStorage {
    fn load_all(&self) -> Result<Vec<(String, ServerStatus)>> {
        let tree = self.tree()?;
        let mut out = Vec::new();
        for kv in tree.iter() {
            let (k, v) = kv.context("迭代 mcp_status tree 失败")?;
            let key_str = std::str::from_utf8(&k)
                .context("mcp_status key 不是合法 UTF-8")?;
            // 跳过非 server: 前缀的元数据 key（预留扩展）
            let Some(name) = key_str.strip_prefix(SERVER_KEY_PREFIX) else {
                continue;
            };
            let status: ServerStatus = serde_json::from_slice(&v)
                .with_context(|| format!("解析 mcp_status '{}' 失败", name))?;
            out.push((name.to_string(), status));
        }
        Ok(out)
    }

    fn get(&self, name: &str) -> Result<Option<ServerStatus>> {
        let tree = self.tree()?;
        self.store
            .get_json(&tree, &status_key(name))
            .context("读取 MCP 状态失败")
    }

    fn record(&self, name: &str, status: ServerStatus) -> Result<()> {
        let tree = self.tree()?;
        self.store
            .insert_json(&tree, &status_key(name), &status)
            .with_context(|| format!("保存 MCP 状态到 KV 失败: server='{}'", name))?;
        // 显式 flush，保证崩溃后状态不丢
        self.store.flush().context("flush KV 失败")?;
        tracing::debug!("MCP 状态已记录: server='{}'", name);
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        let tree = self.tree()?;
        let removed = self
            .store
            .remove(&tree, &status_key(name))
            .context("删除 KV 状态失败")?;
        if removed {
            self.store.flush().context("flush KV 失败")?;
            tracing::info!("MCP 状态已删除: server='{}'", name);
        } else {
            tracing::debug!("MCP 状态删除 no-op（不存在）: server='{}'", name);
        }
        Ok(())
    }

    fn has_status(&self, name: &str) -> bool {
        self.get(name).map(|opt| opt.is_some()).unwrap_or(false)
    }
}

