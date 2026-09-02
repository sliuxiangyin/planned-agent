//! SeaORM migration 入口 —— 启动时 `Migrator::up(&db, None)` 幂等应用全部迁移。

use sea_orm_migration::prelude::*;

mod m20260801_create_chat_messages;
mod m20260801_create_plans;
mod m20260801_create_plans_flexible;
mod m20260801_create_tests;
mod m20260901_create_sessions;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260801_create_tests::Migration),
            Box::new(m20260801_create_plans::Migration),
            // sessions 先建：chat_messages / plans_flexible 以 session_id 外键依赖它
            Box::new(m20260901_create_sessions::Migration),
            Box::new(m20260801_create_plans_flexible::Migration),
            Box::new(m20260801_create_chat_messages::Migration),
        ]
    }
}
