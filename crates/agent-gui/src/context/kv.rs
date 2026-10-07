//! KV 缓存模块 GUI 适配层
//!
//! 启动流程：
//!   1. 解析 sled 数据目录（由全局 `cache_root` 派生；`PLANNED_AGENT_CACHE_PATH` 可整值覆盖）
//!   2. `KvStore::open(&cfg)` 打开 sled（同步阻塞 IO，必须 `spawn_blocking`）
//!   3. 默认 tree 已预打开，业务 tree 首次访问时懒打开
//!
//! 失败仅 warn（不 panic）；调用方通过 `InitStatus.kv.state` 反映。

use std::path::PathBuf;
use std::sync::Arc;

use crate::cache::KvStore;
use crate::config::GuiCacheConfig;
use crate::paths;

/// GUI 层 KV 缓存上下文
///
/// 组件通过 `use_context::<Resource<Option<Arc<KvContext>>>>()` 获取，
/// 再调用 `ctx.store.open_tree(...)` / `ctx.store.default_tree()` 等方法。
pub struct KvContext {
    /// sled 封装句柄（共享所有权）
    pub store: Arc<KvStore>,
}

impl KvContext {
    /// 从配置异步初始化 KV 缓存
    ///
    /// `path` 已由 [`GuiConfig::kv_path`](crate::config::GuiConfig::kv_path) 解析好（含全局
    /// `cache_root`）—— 本层不认识 `cache_root`（见 `docs/planned-agent/gui-cache-root.md` §3.4）。
    ///
    /// `sled::Config::open()` 是同步阻塞 IO；通过 `spawn_blocking` 卸载到阻塞线程池，
    /// 避免阻塞 tokio reactor。
    pub async fn init(config: &GuiCacheConfig, path: PathBuf) -> anyhow::Result<Self> {
        // 父目录（即 `cache_root`）由宿主保证；sled 自己建目标目录，故这里只建父
        paths::ensure_parent(&path)?;

        // 把 path 在 spawn_blocking 前物化，避免 &str 生命周期跨 await
        let cfg = GuiCacheConfig {
            path: path.to_string_lossy().into_owned(),
            ..config.clone()
        };

        let store = tokio::task::spawn_blocking(move || KvStore::open(&cfg))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {}", e))??;

        let stats = store.stats()?;
        tracing::info!(
            "KV 缓存初始化完成: path={}, size_on_disk={}B, tree_count={}",
            path.display(),
            stats.size_on_disk_bytes,
            stats.tree_count,
        );

        Ok(Self {
            store: Arc::new(store),
        })
    }
}