//! SeaORM migration 入口 —— 启动时 `Migrator::up(&db, None)` 幂等应用全部迁移。

use sea_orm_migration::prelude::*;

mod m20260101_000001_create_tests;
mod m20260806_add_plans_flexible_output_schema;
mod m20260806_create_plans_and_messages;
mod m20260806_create_plans_flexible;
mod m20260807_add_plans_flexible_input_schema;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260101_000001_create_tests::Migration),
            Box::new(m20260806_create_plans_and_messages::Migration),
            Box::new(m20260806_create_plans_flexible::Migration),
            Box::new(m20260806_add_plans_flexible_output_schema::Migration),
            Box::new(m20260807_add_plans_flexible_input_schema::Migration),
        ]
    }
}