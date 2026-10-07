//! 本地 KV 缓存配置（sled 后端，`[cache]`）。

use serde::{Deserialize, Serialize};

/// 本地 KV 缓存配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiCacheConfig {
    /// sled 数据目录（启动时若不存在则自动创建）—— 默认是 `cache_root` 下的**单段名** `kv_store`。
    #[serde(default = "default_cache_path")]
    pub path: String,

    /// 内存缓存字节数（透传 `sled::Config::cache_capacity`；越大热点越快，OOM 风险越高）
    #[serde(default = "default_cache_capacity")]
    pub cache_capacity: u64,

    /// 自动 flush 间隔（毫秒；0 = 关闭后台 flush，依赖 drop 时 flush）
    #[serde(default = "default_cache_flush_interval_ms")]
    pub flush_interval_ms: u64,
}

fn default_cache_path() -> String {
    "kv_store".to_string()
}

fn default_cache_capacity() -> u64 {
    128 * 1024 * 1024 // 128 MiB
}

fn default_cache_flush_interval_ms() -> u64 {
    1000
}

impl Default for GuiCacheConfig {
    fn default() -> Self {
        Self {
            path: default_cache_path(),
            cache_capacity: default_cache_capacity(),
            flush_interval_ms: default_cache_flush_interval_ms(),
        }
    }
}
