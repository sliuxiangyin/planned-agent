//! `McpManager` 持久化侧 impl —— ① 服务 / 配置 CRUD（config + tools cache）
//!
//! 经内部 `bundle` 聚合的 `McpConfigStorage` 完成增删改查，**不连接任何 server**。
//! `delete_server` 额外联动清理运行时路由表（`routing::clear_server_state`）。

use anyhow::Result;

use crate::config::{McpConfigFile, McpServerEntry};

use super::McpManager;

impl McpManager {
    /// 加载完整 config（绕过 join，给 CLI / 原始数据场景）
    pub fn load_config(&self) -> Result<McpConfigFile> {
        self.bundle.load_config()
    }

    /// 新增 server（仅持久化，不连接）
    pub fn add_server(&self, entry: McpServerEntry) -> Result<McpConfigFile> {
        self.bundle.config_manager().add_server(entry)
    }

    /// 更新 server（仅持久化，不连接）
    pub fn update_server(&self, name: &str, entry: McpServerEntry) -> Result<McpConfigFile> {
        self.bundle.config_manager().update_server(name, entry)
    }

    /// 删除 server（config + status 联动清理，并清空运行时路由表）
    pub fn delete_server(&self, name: &str) -> Result<McpConfigFile> {
        let cfg = self.bundle.delete_server(name)?;
        self.clear_server_state(name);
        Ok(cfg)
    }
}
