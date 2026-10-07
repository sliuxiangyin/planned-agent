//! 本地持久化配置（SQLite via SeaORM，`[storage]`）。

use serde::{Deserialize, Serialize};

/// 本地持久化配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiStorageConfig {
    /// SQLite 数据库文件路径 —— 默认是 `cache_root` 下的**单段名** `agent-gui.db`
    /// （解析规则见 `docs/planned-agent/gui-cache-root.md` §3.3）。
    #[serde(default = "default_storage_db_path")]
    pub db_path: String,

    /// 启动时打印 schema 概要（仅调试）
    #[serde(default)]
    pub echo_schema: bool,
}

fn default_storage_db_path() -> String {
    "agent-gui.db".to_string()
}

impl Default for GuiStorageConfig {
    fn default() -> Self {
        Self {
            db_path: default_storage_db_path(),
            echo_schema: false,
        }
    }
}
