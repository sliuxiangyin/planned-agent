//! `KvStore`：sled 薄封装 + JSON 序列化工具方法
//!
//! 设计要点：
//! - 仅在 init 时打开默认 tree，避免每次调用付一次 open 开销
//! - 业务 tree 通过 [`KvStore::open_tree`] 按需懒打开（sled 内部会复用句柄）
//! - 序列化走 `serde_json`（人类可读、便于调试；后续可替换为 bincode）
//!
//! 不依赖 Dioxus / tokio，可在任意上下文使用。

use serde::{de::DeserializeOwned, Serialize};
use sled::{Db, Tree};

use super::error::{CacheError, CacheResult};
use crate::config::GuiCacheConfig;

/// 默认 tree 名称（懒得起名场景的统一入口）
const DEFAULT_TREE: &str = "default";

/// 最大允许 tree 名长度（防御性约束，避免异常输入污染 sled 内部命名空间）
const MAX_KEY_LEN: usize = 200;

/// sled 数据库 + 默认 tree 句柄
///
/// 由 [`KvStore::open`] 构造；持有 `Arc<KvStore>` 在多线程间共享。
pub struct KvStore {
    db: Db,
    default: Tree,
}

/// KV 缓存运行时统计（sled 0.34 不再暴露统一 `DbStats`，改为按需聚合）
#[derive(Debug, Clone, Copy)]
pub struct KvStats {
    /// 当前 sled 数据目录占用字节数（仅落盘文件大小，不含内存缓存）
    pub size_on_disk_bytes: u64,
    /// 是否从上一次未正常关闭的实例恢复
    pub was_recovered: bool,
    /// 已打开的 tree 数量（含默认 tree）
    pub tree_count: usize,
}

impl KvStore {
    /// 按配置打开 sled 数据库 + 默认 tree
    ///
    /// 调用方需负责父目录已存在（[`crate::context::kv::KvContext::init`] 中处理）。
    pub fn open(config: &GuiCacheConfig) -> CacheResult<Self> {
        let flush_every_ms = if config.flush_interval_ms == 0 {
            None // 0 表示关闭后台 flush，依赖 drop 时 flush
        } else {
            Some(config.flush_interval_ms)
        };

        let db = sled::Config::default()
            .path(&config.path)
            .cache_capacity(config.cache_capacity)
            .flush_every_ms(flush_every_ms)
            .open()?;

        let default = db.open_tree(DEFAULT_TREE)?;
        tracing::info!(
            "KV 缓存已打开: path={}, cache_capacity={}B, flush_interval={}ms",
            config.path,
            config.cache_capacity,
            config.flush_interval_ms
        );

        Ok(Self { db, default })
    }

    /// 暴露底层 [`Db`]（高级用法：事务、scan、range 等）
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// 默认 tree 句柄（适用于"懒得起名"的轻量场景）
    pub fn default_tree(&self) -> &Tree {
        &self.default
    }

    /// 按名称打开业务 tree（同名重复调用走 sled 内部缓存，开销可控）
    pub fn open_tree(&self, name: &str) -> CacheResult<Tree> {
        validate_key(name)?;
        Ok(self.db.open_tree(name)?)
    }

    /// JSON 工具方法：写入（value 走 `serde_json::to_vec`）
    pub fn insert_json<T: Serialize>(
        &self,
        tree: &Tree,
        key: &[u8],
        value: &T,
    ) -> CacheResult<()> {
        let bytes = serde_json::to_vec(value)?;
        tree.insert(key, bytes)?;
        Ok(())
    }

    /// JSON 工具方法：读取（不存在返回 `Ok(None)`）
    pub fn get_json<T: DeserializeOwned>(
        &self,
        tree: &Tree,
        key: &[u8],
    ) -> CacheResult<Option<T>> {
        match tree.get(key)? {
            Some(ivec) => Ok(Some(serde_json::from_slice(&ivec)?)),
            None => Ok(None),
        }
    }

    /// JSON 工具方法：删除（返回是否实际删除了一个值）
    pub fn remove(&self, tree: &Tree, key: &[u8]) -> CacheResult<bool> {
        Ok(tree.remove(key)?.is_some())
    }

    /// 主动 flush（调试 / 关键节点用；正常情况依赖 sled 后台 + drop 自动 flush）
    pub fn flush(&self) -> CacheResult<()> {
        self.db.flush()?;
        Ok(())
    }

    /// 统计信息（聚合 sled 真实存在的 API：落盘大小 / 恢复状态 / tree 数量）
    ///
    /// 注：sled 0.34 移除了旧版 `DbStats`，这里返回项目自定义的精简视图。
    pub fn stats(&self) -> CacheResult<KvStats> {
        Ok(KvStats {
            size_on_disk_bytes: self.db.size_on_disk()?,
            was_recovered: self.db.was_recovered(),
            tree_count: self.db.tree_names().len(),
        })
    }
}

/// 校验 tree 名 / cache key 的合法性
fn validate_key(name: &str) -> CacheResult<()> {
    if name.is_empty() || name.len() > MAX_KEY_LEN {
        return Err(CacheError::InvalidKey(name.to_string()));
    }
    if name.contains('\0') {
        return Err(CacheError::InvalidKey(name.to_string()));
    }
    Ok(())
}