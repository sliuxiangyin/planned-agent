//! `McpManager` 持久化侧 impl —— config + status join 视图
//!
//! 读路径，**无副作用、不触发连接**；供 GUI `list_page` 一次拿到已 join 视图。

use anyhow::Result;

use crate::bundle::McpServerView;

use super::McpManager;

impl McpManager {
    /// 加载所有 server 的统一视图（config + status 已 join）
    pub fn list_servers(&self) -> Result<Vec<McpServerView>> {
        self.bundle.load_servers()
    }

    /// 按 name 读取单个 server 的视图
    pub fn get_server(&self, name: &str) -> Result<Option<McpServerView>> {
        self.bundle.get_server(name)
    }
}
