//! SeaORM migration 入口 —— 启动时 `Migrator::up(&db, None)` 幂等应用全部迁移。

use sea_orm_migration::prelude::*;

mod m20260801_create_chat_messages;
mod m20260801_create_plans;
mod m20260801_create_tests;
mod m20260904_create_flexible_state;
mod m20260910_create_plans_flexible_sessions;
mod m20261005_add_plans_flexible_sessions_revision;
mod m20261005_create_flexible_run_history;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260801_create_tests::Migration),
            Box::new(m20260801_create_plans::Migration),
            // 会话表先建：chat_messages / flexible_state 以 session_id 外键依赖它
            Box::new(m20260910_create_plans_flexible_sessions::Migration),
            Box::new(m20260801_create_chat_messages::Migration),
            Box::new(m20260904_create_flexible_state::Migration),
            // 工具链记忆：先给会话表加 revision（判据列），再建 history 表（外键依赖会话表）
            Box::new(m20261005_add_plans_flexible_sessions_revision::Migration),
            Box::new(m20261005_create_flexible_run_history::Migration),
        ]
    }
}
