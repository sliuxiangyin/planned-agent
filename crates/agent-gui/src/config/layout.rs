//! 全局 `cache_root` 派生的四个落盘路径 —— **拼接唯一落点**。
//!
//! 使用方（`context/kv.rs`、`context/storage.rs`、`context/rag.rs`、`services/run_service.rs`）
//! 只拿结果，不认识 `cache_root`，也不重复写拼接规则。规则本体（单段拼根 / 多段相对按 cwd /
//! 绝对原样）在 [`crate::paths`]；设计见 `docs/planned-agent/gui-cache-root.md`。

use std::path::PathBuf;

use crate::paths;

use super::GuiConfig;

impl GuiConfig {
    /// sled KV 数据目录。
    ///
    /// env `PLANNED_AGENT_CACHE_PATH` 存在则**整值覆盖**（优先于 `cache_root` 派生）。
    pub fn kv_path(&self) -> PathBuf {
        paths::resolve_with_env(&self.cache_root, &self.cache.path, "PLANNED_AGENT_CACHE_PATH")
    }

    /// SQLite 数据库文件路径（env `PLANNED_AGENT_DB_PATH` 整值覆盖）。
    pub fn db_path(&self) -> PathBuf {
        paths::resolve_with_env(&self.cache_root, &self.storage.db_path, "PLANNED_AGENT_DB_PATH")
    }

    /// RAG 向量库目录。
    ///
    /// PolarisDB 自己不解析 cwd，所以这里给的是**绝对路径**。
    pub fn rag_store_path(&self) -> PathBuf {
        paths::resolve_under(&self.cache_root, &self.rag.store.path)
    }

    /// flexible 步骤产出根目录。
    ///
    /// **不含会话段** —— 执行器不认识「会话」概念，会话段由宿主再拼 `/<session_id>`。
    pub fn flexible_output_dir(&self) -> PathBuf {
        paths::resolve_under(&self.cache_root, &self.flexible.output_cache_dir)
    }
}
