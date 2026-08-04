//! MCP 连接状态存储 trait —— 调用方按场景实现后注入 [`McpStatusManager`]
//!
//! ## 设计要点
//!
//! - **与 [`McpConfigStorage`] 对称**：同样的 sync API + `Send + Sync` + 不引入 `async_trait`
//! - **per-server 粒度**：key 用 `server:<name>` 形式（KV 友好），不存单 blob，
//!   避免每次状态更新重写所有 server 的 tools schema
//! - **仅保留最近一次**：不存历史事件流，需要历史时另起 `mcp_events` tree
//! - **大小受限**：`error_message` 由调用方截断（建议 ≤ 200 字符），
//!   `stderr_tail` **禁止**写这里（保留在内存 / dialog 即可）
//!
//! ## 调用方典型用法
//!
//! ```ignore
//! use std::sync::Arc;
//! use planned_agent_mcp_rmcp::{
//!     McpStatusManager, storage::{McpStatusStorage, FileMcpStatusStorage},
//! };
//!
//! // CLI：默认文件存储
//! let mgr = McpStatusManager::new("./data/mcp-status.json");
//!
//! // GUI：注入 KV 实现
//! let storage: Arc<dyn McpStatusStorage> = Arc::new(MyKvStatusStorage::new(...));
//! let mgr = McpStatusManager::with_storage(storage);
//! ```

use anyhow::Result;
use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════════
// 数据模型
// ═══════════════════════════════════════════════════════════════════════

/// 单次连接尝试的最终状态
///
/// 仅记录最近一次，重启后用于"冷启动可见性"：
/// 用户一打开 GUI 就能看到"上次连接失败"，无需盲刷。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LastStatus {
    /// 最近一次连接成功，获取了 `tool_count` 个工具
    Ready {
        tool_count: u32,
    },
    /// 正在连接中（通常 GUI 重启时不会持久化此状态；提供仅作完整性）
    Connecting,
    /// 连接失败
    Failed,
}

/// 单个 server 的最近一次连接快照
///
/// ## 字段约束
///
/// - `error_kind`：机器可读的失败分类（`"timeout"` / `"spawn"` / `"handshake"`），
///   UI 用于差异化展示（颜色 / 图标 / 文案）
/// - `error_message`：人类可读的截断错误消息（不含 stderr_tail）
/// - `attempt_at`：unix epoch 秒，UI 用于判断"陈旧度"
///
/// ## 字段命名约定
///
/// 所有字段都带前缀或后缀（`status` / `error_*` / `attempt_at`），
/// 明确这些是**运行时观察**，不是配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerStatus {
    pub status: LastStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub attempt_at: u64,
}

impl ServerStatus {
    /// 当前 unix epoch 秒（用于 [`ServerStatus::attempt_at`]）
    pub fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// 构造成功状态
    pub fn ready(tool_count: u32, attempt_at: u64) -> Self {
        Self {
            status: LastStatus::Ready { tool_count },
            error_kind: None,
            error_message: None,
            attempt_at,
        }
    }

    /// 构造失败状态
    ///
    /// `error_message` 调用方应自行截断到合理长度（建议 ≤ 200 字符）。
    pub fn failed(error_kind: impl Into<String>, error_message: impl Into<String>, attempt_at: u64) -> Self {
        Self {
            status: LastStatus::Failed,
            error_kind: Some(error_kind.into()),
            error_message: Some(error_message.into()),
            attempt_at,
        }
    }

    /// 构造连接中状态（一般不在持久化路径使用）
    pub fn connecting(attempt_at: u64) -> Self {
        Self {
            status: LastStatus::Connecting,
            error_kind: None,
            error_message: None,
            attempt_at,
        }
    }

    /// 是否处于"陈旧"状态：用于 UI 弱化提示（"上次成功于 X 分钟前"）
    ///
    /// 当前阈值：> 1 小时视为陈旧。
    pub fn is_stale(&self, now_secs: u64) -> bool {
        now_secs.saturating_sub(self.attempt_at) > 3600
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Storage trait
// ═══════════════════════════════════════════════════════════════════════

/// MCP 连接状态存储抽象接口
///
/// 由调用方按场景实现：
/// - **CLI 场景**：[`crate::storage::FileMcpStatusStorage`]（mcp-rmcp 内置）
/// - **GUI 场景**：调用方自实现（如基于 sled KV）
/// - **测试场景**：[`crate::storage::InMemoryMcpStatusStorage`]（mcp-rmcp 内置）
///
/// 所有方法语义对齐"最近一次状态"模型：
/// - `record` 是**覆盖式**，仅保留最近一次
/// - `delete` 用于与 [`McpConfigStorage::delete_server`] 联动，避免残留
/// - `load_all` / `get` 在底层不存在时返回空，不抛错（让 GUI 冷启动优雅降级）
pub trait McpStatusStorage: Send + Sync {
    /// 加载所有 server 的最近状态（启动时给 GUI 灌红/绿/灰）
    ///
    /// 返回 `(server_name, status)` 元组列表。
    /// 底层为空 / 文件不存在时返回 `Ok(vec![])`，**不**自动持久化默认值。
    fn load_all(&self) -> Result<Vec<(String, ServerStatus)>>;

    /// 读取单个 server 的最近状态；无记录返回 `Ok(None)`
    fn get(&self, name: &str) -> Result<Option<ServerStatus>>;

    /// 记录一次连接尝试的结果（覆盖式）
    fn record(&self, name: &str, status: ServerStatus) -> Result<()>;

    /// 删除指定 server 的状态（与 [`McpConfigStorage::delete_server`] 联动）
    fn delete(&self, name: &str) -> Result<()>;

    /// 检查指定 server 是否有状态记录
    fn has_status(&self, name: &str) -> bool;
}
