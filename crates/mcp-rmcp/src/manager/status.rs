//! `McpManager` 持久化侧 impl —— ③ 连接状态读写（config + status storage）
//!
//! 状态由写路径（连接成功 / 失败 / 预载）内部自动维护；此处提供读写与结构化失败落库。

use anyhow::Result;
use planned_agent_core::mcp::types::ConnectionError;

use crate::storage::ServerStatus;

use super::McpManager;

impl McpManager {
    /// 记录一次连接状态（覆盖式；写失败仅告警不抛错）
    pub fn record_status(&self, name: &str, status: ServerStatus) -> Result<()> {
        self.bundle.record_status(name, status)
    }

    /// 读取单个 server 的最近状态
    pub fn get_status(&self, name: &str) -> Result<Option<ServerStatus>> {
        self.bundle.get_status(name)
    }

    /// 加载所有 server 的最近状态
    pub fn list_status(&self) -> Result<Vec<(String, ServerStatus)>> {
        self.bundle.load_all_statuses()
    }

    /// 删除指定 server 的状态
    pub fn delete_status(&self, name: &str) -> Result<()> {
        self.bundle.delete_status(name)
    }

    /// 检查指定 server 是否有状态记录
    pub fn has_status(&self, name: &str) -> bool {
        self.bundle.has_status(name)
    }

    /// 把 `ConnectionError` 直接落库（结构化 Failed）
    pub fn record_failure(&self, server_name: &str, conn_err: &ConnectionError) {
        self.bundle.record_failure(server_name, conn_err)
    }
}
