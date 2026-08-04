//! KV 缓存统一错误类型（sled 后端）

use thiserror::Error;

/// KV 缓存模块错误
#[derive(Debug, Error)]
pub enum CacheError {
    /// sled 内部错误（IO、配置、tree open 等）
    #[error("sled error: {0}")]
    Sled(#[from] sled::Error),

    /// serde_json 序列化/反序列化错误
    #[error("serde_json error: {0}")]
    Serde(#[from] serde_json::Error),

    /// 非法 tree/key 命名（空串 / 含 NUL / 过长）
    #[error("invalid cache key: {0}")]
    InvalidKey(String),
}

/// KV 缓存模块统一 Result 别名
pub type CacheResult<T> = Result<T, CacheError>;