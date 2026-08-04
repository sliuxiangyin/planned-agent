//! Storage 模块 GUI 适配层
//!
//! 启动流程：
//!   1. 解析 DB 文件路径（多候选 + 自动 mkdir 父目录）
//!   2. `Database::connect("sqlite://...?mode=rwc")` 建立连接
//!   3. `Migrator::up(&db, None)` 应用全部 pending 迁移
//!   4. 构造 1 个 `TestRepo`（MVP 阶段仅测试用）
//!
//! 失败仅 warn（不 panic）；调用方通过 `InitStatus.storage.state` 反映。

use std::path::PathBuf;
use std::sync::Arc;

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use crate::config::GuiStorageConfig;
use crate::storage::{migrations::Migrator, repository::TestRepo};

/// GUI 层 Storage 上下文
///
/// 组件通过 `use_context::<Resource<Option<Arc<StorageContext>>>>()` 获取，
/// MVP 阶段仅暴露 `ctx.test_repo` 用于验证 Repository ↔ Entity 链路。
#[allow(dead_code)] // MVP 占位 —— 阶段 2 业务接入时启用
pub struct StorageContext {
    /// SQLite 连接（SeaORM DatabaseConnection）
    pub db: DatabaseConnection,
    /// tests 表仓库（验证用）
    pub test_repo: Arc<TestRepo>,
}

impl StorageContext {
    /// 从配置异步初始化 SQLite + 迁移 + TestRepo
    pub async fn init(config: &GuiStorageConfig) -> anyhow::Result<Self> {
        let path = resolve_db_path(&config.db_path)?;
        let url = format!("sqlite://{}?mode=rwc", path.display());

        let mut opt = ConnectOptions::new(url);
        opt.max_connections(8).min_connections(1).sqlx_logging(false);
        if config.echo_schema {
            opt.sqlx_logging_level(tracing::log::LevelFilter::Info);
        }

        let db: DatabaseConnection = Database::connect(opt).await?;
        tracing::info!("Storage SQLite 已连接: {}", path.display());

        // 应用全部 pending 迁移（幂等）
        Migrator::up(&db, None).await?;
        tracing::info!("Storage SQLite 迁移完成");

        Ok(Self {
            test_repo: Arc::new(TestRepo::new(db.clone())),
            db,
        })
    }
}

/// 解析 DB 文件路径：复用 config.rs try_load 的多候选模式
///
/// 优先级：
///   1. 环境变量 `PLANNED_AGENT_DB_PATH`（最高优先）
///   2. 配置里写的 `db_path`（相对路径以 cwd 为基准）
///   3. 配置路径的父目录自动 mkdir
fn resolve_db_path(configured: &str) -> anyhow::Result<PathBuf> {
    let raw = std::env::var("PLANNED_AGENT_DB_PATH").unwrap_or_else(|_| configured.to_string());

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