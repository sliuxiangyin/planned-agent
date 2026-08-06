//! plans_flexible 表迁移。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // plans 表：添加 flexible_version 列
        manager
            .alter_table(
                Table::alter()
                    .table(Plans::Table)
                    .add_column(
                        ColumnDef::new(Plans::FlexibleVersion)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        // plans_flexible 表
        manager
            .create_table(
                Table::create()
                    .table(PlansFlexible::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PlansFlexible::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PlansFlexible::PlanId)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PlansFlexible::Version)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PlansFlexible::PreviousSummary)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .col(
                        ColumnDef::new(PlansFlexible::Todos)
                            .string()
                            .not_null()
                            .default("[]"),
                    )
                    .col(
                        ColumnDef::new(PlansFlexible::Params)
                            .string()
                            .not_null()
                            .default("[]"),
                    )
                    .col(
                        ColumnDef::new(PlansFlexible::CreatedAt)
                            .string()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_plans_flexible_plan_id")
                            .from(PlansFlexible::Table, PlansFlexible::PlanId)
                            .to(Plans::Table, Plans::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // 索引：plan_id + version 联合查询
        manager
            .create_index(
                Index::create()
                    .name("idx_plans_flexible_plan_version")
                    .table(PlansFlexible::Table)
                    .col(PlansFlexible::PlanId)
                    .col(PlansFlexible::Version)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop().table(PlansFlexible::Table).to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Plans::Table)
                    .drop_column(Plans::FlexibleVersion)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Plans {
    Table,
    Id,
    FlexibleVersion,
}

#[derive(DeriveIden)]
enum PlansFlexible {
    Table,
    Id,
    PlanId,
    Version,
    PreviousSummary,
    Todos,
    Params,
    CreatedAt,
}
