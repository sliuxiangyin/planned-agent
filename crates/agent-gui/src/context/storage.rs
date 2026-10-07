//! Storage 模块 GUI 适配层
//!
//! 启动流程：
//!   1. 解析 DB 文件路径（由全局 `cache_root` 派生；`PLANNED_AGENT_DB_PATH` 可整值覆盖）
//!   2. `Database::connect("sqlite://...?mode=rwc")` 建立连接
//!   3. `Migrator::up(&db, None)` 应用全部 pending 迁移
//!   4. 构造 Repo 实例
//!
//! 失败仅 warn（不 panic）；调用方通过 `InitStatus.storage.state` 反映。

use std::path::PathBuf;
use std::sync::Arc;

use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use crate::config::GuiStorageConfig;
use crate::paths;
use crate::storage::{
    migrations::Migrator,
    repository::{
        ChatMessageRepo, FlexibleRunHistoryRepo, FlexibleStateRepo, PlanRepo,
        PlansFlexibleSessionsRepo, TestRepo,
    },
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
    /// flexible_state 表仓库（灵活模式流程中间状态）
    flexible_state_repo: Arc<FlexibleStateRepo>,
    /// plans_flexible_sessions 表仓库（「会话即版本」：会话/版本列表）
    plans_flexible_sessions_repo: Arc<PlansFlexibleSessionsRepo>,
    /// flexible_run_history 表仓库（每步「上次成功用过的工具链」记忆）
    flexible_run_history_repo: Arc<FlexibleRunHistoryRepo>,
}

impl StorageContext {
    pub fn test_repo(&self) -> Arc<TestRepo> { self.test_repo.clone() }
    pub fn plan_repo(&self) -> Arc<PlanRepo> { self.plan_repo.clone() }
    pub fn chat_message_repo(&self) -> Arc<ChatMessageRepo> { self.chat_message_repo.clone() }
    pub fn flexible_state_repo(&self) -> Arc<FlexibleStateRepo> { self.flexible_state_repo.clone() }
    pub fn plans_flexible_sessions_repo(&self) -> Arc<PlansFlexibleSessionsRepo> {
        self.plans_flexible_sessions_repo.clone()
    }
    /// flexible_run_history 表仓库（阶段 1 已建表；注入侧接线前先不读）
    #[allow(dead_code)]
    pub fn flexible_run_history_repo(&self) -> Arc<FlexibleRunHistoryRepo> {
        self.flexible_run_history_repo.clone()
    }

    /// 从配置异步初始化 SQLite + 迁移 + Repos
    ///
    /// `path` 已由 [`GuiConfig::db_path`](crate::config::GuiConfig::db_path) 解析好（含全局
    /// `cache_root`）—— 本层不认识 `cache_root`（见 `docs/planned-agent/gui-cache-root.md` §3.4）。
    pub async fn init(config: &GuiStorageConfig, path: PathBuf) -> anyhow::Result<Self> {
        // 父目录（即 `cache_root`）由宿主保证；SQLite 以 `mode=rwc` 自建文件
        paths::ensure_parent(&path)?;
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
            flexible_state_repo: Arc::new(FlexibleStateRepo::new(db.clone())),
            plans_flexible_sessions_repo: Arc::new(PlansFlexibleSessionsRepo::new(db.clone())),
            flexible_run_history_repo: Arc::new(FlexibleRunHistoryRepo::new(db.clone())),
        })
    }
}
