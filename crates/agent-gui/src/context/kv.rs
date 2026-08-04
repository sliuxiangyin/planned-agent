//! KV 缓存模块 GUI 适配层
//!
//! 启动流程：
//!   1. 解析 sled 数据目录路径（多候选 + 自动 mkdir）
//!   2. `KvStore::open(&cfg)` 打开 sled（同步阻塞 IO，必须 `spawn_blocking`）
//!   3. 默认 tree 已预打开，业务 tree 首次访问时懒打开
//!
//! 失败仅 warn（不 panic）；调用方通过 `InitStatus.kv.state` 反映。

use std::path::PathBuf;
use std::sync::Arc;

use crate::cache::KvStore;
use crate::config::GuiCacheConfig;

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
    /// `sled::Config::open()` 是同步阻塞 IO；通过 `spawn_blocking` 卸载到阻塞线程池，
    /// 避免阻塞 tokio reactor。
    pub async fn init(config: &GuiCacheConfig) -> anyhow::Result<Self> {
        let path = resolve_cache_path(&config.path)?;
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

/// 解析 sled 数据目录路径
///
/// 优先级：
///   1. 环境变量 `PLANNED_AGENT_CACHE_PATH`（最高）
///   2. 配置里写的 `path`（相对路径以 cwd 为基准）
///   3. 父目录自动 mkdir
fn resolve_cache_path(configured: &str) -> anyhow::Result<PathBuf> {
    let raw = std::env::var("PLANNED_AGENT_CACHE_PATH").unwrap_or_else(|_| configured.to_string());

    let path = PathBuf::from(&raw);
    let path = if path.is_relative() {
        if let Ok(cwd) = std::env::current_dir() {
            cwd.join(&path)
        } else {
            path
        }
    } else {
        path
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(path)
}