//! plans + messages 表迁移。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // plans 表
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
                        ColumnDef::new(Plans::Todos)
                            .string()
                            .not_null()
                            .default("[]"),
                    )
                    .col(ColumnDef::new(Plans::CreatedAt).string().not_null())
                    .col(ColumnDef::new(Plans::UpdatedAt).string().not_null())
                    .to_owned(),
            )
            .await?;

        // messages 表
        manager
            .create_table(
                Table::create()
                    .table(Messages::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Messages::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Messages::PlanId).string().not_null())
                    .col(ColumnDef::new(Messages::Role).string().not_null())
                    .col(
                        ColumnDef::new(Messages::Content)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .col(ColumnDef::new(Messages::CreatedAt).string().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_messages_plan_id")
                            .from(Messages::Table, Messages::PlanId)
                            .to(Plans::Table, Plans::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // 索引
        manager
            .create_index(
                Index::create()
                    .name("idx_messages_plan_id")
                    .table(Messages::Table)
                    .col(Messages::PlanId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Messages::Table).to_owned())
            .await?;
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
    Todos,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Messages {
    Table,
    Id,
    PlanId,
    Role,
    Content,
    CreatedAt,
}
