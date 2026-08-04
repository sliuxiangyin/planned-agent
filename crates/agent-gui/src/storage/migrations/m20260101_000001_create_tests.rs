//! tests 表迁移 —— 仅用于验证 sea-orm-migration 框架接入。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.create_table(
            Table::create()
                .table(Tests::Table)
                .col(
                    ColumnDef::new(Tests::Id)
                        .integer()
                        .not_null()
                        .auto_increment()
                        .primary_key(),
                )
                .col(ColumnDef::new(Tests::Name).string().not_null())
                .col(ColumnDef::new(Tests::Value).string().not_null())
                .col(ColumnDef::new(Tests::CreatedAt).big_integer().not_null())
                .to_owned(),
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Tests::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
pub enum Tests {
    Table,
    Id,
    Name,
    Value,
    CreatedAt,
}