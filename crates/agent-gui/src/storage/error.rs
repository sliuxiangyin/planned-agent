//! 持久化层错误类型
//!
//! 包装 SeaORM `DbErr`，便于仓库层统一向上抛。

use sea_orm::DbErr;

/// Storage 统一错误
#[allow(dead_code)] // MVP 占位 —— 阶段 2 业务接入时启用
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("SeaORM error: {0}")]
    Db(#[from] DbErr),

    #[error("record not found: {0}")]
    NotFound(String),

    #[error("path error: {0}")]
    Path(String),

    #[error("migration error: {0}")]
    Migration(String),
}

#[allow(dead_code)] // MVP 占位 —— 阶段 2 业务接入时启用
pub type StorageResult<T> = std::result::Result<T, StorageError>;