//! Storage 模块 GUI 适配层
//!
//! 启动流程：
//!   1. 解析 DB 文件路径（多候选 + 自动 mkdir 父目录）
//!   2. `Database::connect("sqlite://...?mode=rwc")` 建立连接
//!   3. `Migrator::up(&db, None)` 应用全部 pending 迁移
//!   4. 构造 Repo 实例
//!
//! 失败仅 warn（不 panic）；调用方通过 `InitStatus.storage.state` 反映。

use std::path::PathBuf;
use std::sync::Arc;

use dioxus::prelude::*;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use crate::config::GuiStorageConfig;
use crate::storage::{
    migrations::Migrator,
    repository::{ChatMessageRepo, PlanRepo, PlansFlexibleRepo, TestRepo},
};

/// GUI 层 Storage 上下文
///
/// 组件通过 `use_context::<Resource<Option<Arc<StorageContext>>>>()` 获取。
#[allow(dead_code)]
pub struct StorageContext {
    /// SQLite 连接（SeaORM DatabaseConnection）
    pub db: DatabaseConnection,
    /// tests 表仓库（验证用）
    test_repo: Arc<TestRepo>,
    /// plans 表仓库
    plan_repo: Arc<PlanRepo>,
    /// chat_messages 表仓库（灵活模式聊天消息）
    chat_message_repo: Arc<ChatMessageRepo>,
    /// plans_flexible 表仓库（灵活模式计划版本快照）
    plans_flexible_repo: Arc<PlansFlexibleRepo>,
}

impl StorageContext {
    pub fn test_repo(&self) -> Arc<TestRepo> { self.test_repo.clone() }
    pub fn plan_repo(&self) -> Arc<PlanRepo> { self.plan_repo.clone() }
    pub fn chat_message_repo(&self) -> Arc<ChatMessageRepo> { self.chat_message_repo.clone() }
    pub fn plans_flexible_repo(&self) -> Arc<PlansFlexibleRepo> { self.plans_flexible_repo.clone() }

    /// 从配置异步初始化 SQLite + 迁移 + Repos
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
            db: db.clone(),
            test_repo: Arc::new(TestRepo::new(db.clone())),
            plan_repo: Arc::new(PlanRepo::new(db.clone())),
            chat_message_repo: Arc::new(ChatMessageRepo::new(db.clone())),
            plans_flexible_repo: Arc::new(PlansFlexibleRepo::new(db.clone())),
        })
    }
}

/// 解析 DB 文件路径：复用 config.rs try_load 的多候选模式
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

/// 从 `Resource<Option<Arc<StorageContext>>>` 中提取仓库，简化调用链。
pub fn storage_repo<F, T>(
    storage: Resource<Option<Arc<StorageContext>>>,
    f: F,
) -> Option<Arc<T>>
where
    F: FnOnce(&StorageContext) -> Arc<T>,
{
    let guard = storage.read();
    let inner: &Option<Option<Arc<StorageContext>>> = &*guard;
    inner.as_ref().and_then(|opt| opt.as_ref()).map(|ctx| f(ctx))
}
