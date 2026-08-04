//! SeaORM migration 入口 —— 启动时 `Migrator::up(&db, None)` 幂等应用全部迁移。

use sea_orm_migration::prelude::*;

mod m20260101_000001_create_tests;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260101_000001_create_tests::Migration)]
    }
}