use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP 工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// MCP 工具调用结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: Value,
    pub is_error: bool,
}

/// MCP 服务器配置（支持多个）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub server_command: String,
    pub server_args: Vec<String>,
    pub transport: String,
    pub timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
    pub is_default: bool,
    pub tools_filter: Option<Vec<String>>,
    /// 工具分类（可选，用于分类过滤）
    #[serde(default)]
    pub categories: Option<Vec<String>>,
}

/// 连接状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub last_ping: Option<chrono::DateTime<chrono::Utc>>,
    pub error_count: u32,
    pub uptime_secs: u64,
    /// 最近一次连接失败的结构化错误（成功连接后会被清空）。
    /// UI 层据此展示失败原因；当前未消费，保留作为数据出口。
    #[serde(default)]
    pub last_error: Option<ConnectionError>,
}

/// 连接失败原因分类
///
/// 用途：MCP 客户端 `connect()` 失败时按失败阶段分类记录，
/// 供上层（UI、监控、Agent 自愈）按类别决定处理策略。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionError {
    /// 启动/握手总耗时超过 `timeout_secs`。
    /// 覆盖完整冷启动链：spawn 子进程 → npx 拉包 → 真实 MCP server 启动 → initialize 握手。
    /// 首次 `npx -y <pkg>` 下载可能耗时数十秒，需要给足余量。
    Timeout {
        /// 实际等待的秒数
        elapsed_secs: u64,
        /// 配置的超时上限（秒）
        timeout_secs: u64,
        /// 子进程 stderr 末尾输出（如果可读且非空）。
        /// 当子进程启动后被超时打断时，这里通常包含真实的报错原因（如 `MODULE_NOT_FOUND`）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_tail: Option<String>,
    },
    /// 无法启动子进程：命令不存在、权限不足等。
    /// 此阶段子进程未运行，无 stderr 可捕获。
    Spawn {
        reason: String,
    },
    /// 进程已启动但 MCP initialize 握手失败（如子进程崩溃、协议错误）。
    /// `stderr_tail` 携带子进程 stderr 末尾输出，便于 UI 展示真实失败原因。
    Handshake {
        reason: String,
        /// 子进程 stderr 末尾输出（如果可读且非空）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_tail: Option<String>,
    },
}

impl ConnectionError {
    /// 机器可读的失败分类（用于持久化 / IPC / UI 标签）
    ///
    /// 返回字符串与 serde `tag` 字段保持一致（`"timeout"` / `"spawn"` / `"handshake"`），
    /// 便于后续 JSON 字段直读。
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Timeout { .. } => "timeout",
            Self::Spawn { .. } => "spawn",
            Self::Handshake { .. } => "handshake",
        }
    }

    /// 供 UI / 日志展示的人类可读消息
    pub fn message(&self) -> String {
        let base = match self {
            Self::Timeout { elapsed_secs, timeout_secs, .. } => format!(
                "MCP server startup timed out after {}s (limit {}s). \
                 npx package download or handshake may be slow; \
                 consider raising `timeout_secs` in the server config.",
                elapsed_secs, timeout_secs
            ),
            Self::Spawn { reason } => {
                format!("Failed to spawn MCP server process: {}", reason)
            }
            Self::Handshake { reason, .. } => {
                format!("MCP handshake failed: {}", reason)
            }
        };

        // 追加子进程 stderr 末尾（让用户看到真实失败原因）
        let stderr = match self {
            Self::Timeout { stderr_tail, .. } => stderr_tail.as_ref(),
            Self::Handshake { stderr_tail, .. } => stderr_tail.as_ref(),
            Self::Spawn { .. } => None,
        };
        match stderr {
            Some(s) if !s.trim().is_empty() => {
                format!("{}\n\n--- subprocess stderr ---\n{}", base, s.trim_end())
            }
            _ => base,
        }
    }
}
