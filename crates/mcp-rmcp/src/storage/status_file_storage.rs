//! 文件存储实现：[`McpStatusStorage`] 的默认后端
//!
//! ## 存储格式
//!
//! 单个 JSON 文件（默认 `./data/mcp-status.json`）：
//! ```json
//! {
//!   "version": 1,
//!   "statuses": {
//!     "playwright": { "status": {"kind": "ready", "tool_count": 24}, "attempt_at": 1722784800 },
//!     "desktop-commander": { "status": {"kind": "failed"}, "error_kind": "handshake", ... }
//!   }
//! }
//! ```
//!
//! - 单文件 + dict 结构，与 `FileMcpConfigStorage` 的"整 blob 写"风格对齐
//! - 文件不存在时返回空 map，**不**自动落盘（避免 KV / 文件污染）
//! - 写入用 `tmp + rename` 原子语义

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::storage::status_trait::{McpStatusStorage, ServerStatus};

/// mcp-status.json 文件根结构
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StatusFile {
    /// schema 版本号（未来字段演进的兼容位）
    #[serde(default = "default_version")]
    version: u32,
    /// server name → ServerStatus 映射
    #[serde(default)]
    statuses: BTreeMap<String, ServerStatus>,
}

fn default_version() -> u32 {
    1
}

impl Default for StatusFile {
    fn default() -> Self {
        Self {
            version: default_version(),
            statuses: BTreeMap::new(),
        }
    }
}

/// 基于 JSON 文件的 [`McpStatusStorage`] 实现（CLI 场景默认）
pub struct FileMcpStatusStorage {
    status_path: String,
}

impl FileMcpStatusStorage {
    /// 默认状态文件路径（CLI 场景）
    pub const DEFAULT_PATH: &'static str = "./data/mcp-status.json";

    /// 构造文件存储；调用方负责保证 `path` 的父目录可写
    pub fn new(status_path: &str) -> Self {
        Self {
            status_path: status_path.to_string(),
        }
    }

    /// 原子写入：写到 `<path>.tmp` 后 `rename` 到目标
    fn save_atomic(&self, file: &StatusFile) -> Result<()> {
        let tmp_path = format!("{}.tmp", self.status_path);
        let json = serde_json::to_string_pretty(file).context("序列化 MCP 状态失败")?;
        std::fs::write(&tmp_path, &json)
            .with_context(|| format!("写入临时文件失败: {}", tmp_path))?;
        std::fs::rename(&tmp_path, &self.status_path)
            .with_context(|| format!("原子重命名失败: {} -> {}", tmp_path, self.status_path))?;
        Ok(())
    }

    fn load_or_default(&self) -> Result<StatusFile> {
        if !Path::new(&self.status_path).exists() {
            return Ok(StatusFile::default());
        }
        let content = std::fs::read_to_string(&self.status_path)
            .with_context(|| format!("读取 MCP 状态文件失败: {}", self.status_path))?;
        let file: StatusFile = serde_json::from_str(&content).unwrap_or_else(|e| {
            // 解析失败：降级为空（避免 GUI 冷启动崩），
            // 但要打 warn 提示用户文件可能损坏
            tracing::warn!(
                "MCP 状态文件解析失败，按空状态启动: path={}, err={}",
                self.status_path,
                e
            );
            StatusFile::default()
        });
        Ok(file)
    }
}

impl McpStatusStorage for FileMcpStatusStorage {
    fn load_all(&self) -> Result<Vec<(String, ServerStatus)>> {
        let file = self.load_or_default()?;
        Ok(file.statuses.into_iter().collect())
    }

    fn get(&self, name: &str) -> Result<Option<ServerStatus>> {
        let file = self.load_or_default()?;
        Ok(file.statuses.get(name).cloned())
    }

    fn record(&self, name: &str, status: ServerStatus) -> Result<()> {
        let mut file = self.load_or_default()?;
        file.statuses.insert(name.to_string(), status);
        self.save_atomic(&file)?;
        info!("MCP 状态已记录: server='{}'", name);
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        let mut file = self.load_or_default()?;
        if file.statuses.remove(name).is_some() {
            self.save_atomic(&file)?;
            info!("MCP 状态已删除: server='{}'", name);
        }
        Ok(())
    }

    fn has_status(&self, name: &str) -> bool {
        self.load_or_default()
            .map(|f| f.statuses.contains_key(name))
            .unwrap_or(false)
    }
}

