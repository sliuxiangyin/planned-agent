//! plans 表迁移（含 flexible_version）。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Plans::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Plans::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Plans::Name).string().not_null())
                    .col(
                        ColumnDef::new(Plans::Description)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .col(ColumnDef::new(Plans::Mode).string().not_null())
                    .col(ColumnDef::new(Plans::Status).string().not_null())
                    .col(
                        ColumnDef::new(Plans::FlexibleVersion)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(Plans::CreatedAt).string().not_null())
                    .col(ColumnDef::new(Plans::UpdatedAt).string().not_null())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Plans::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Plans {
    Table,
    Id,
    Name,
    Description,
    Mode,
    Status,
    FlexibleVersion,
    CreatedAt,
    UpdatedAt,
}
