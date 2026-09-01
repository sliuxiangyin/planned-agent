//! plans_flexible 表迁移（存储 flexible_step5 生成的混合模板）。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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
                    .col(ColumnDef::new(PlansFlexible::PlanId).string().not_null())
                    .col(ColumnDef::new(PlansFlexible::Version).integer().not_null())
                    .col(
                        ColumnDef::new(PlansFlexible::InputSchema)
                            .string()
                            .not_null()
                            .default("{}"),
                    )
                    .col(
                        ColumnDef::new(PlansFlexible::Output)
                            .string()
                            .not_null()
                            .default("{}"),
                    )
                    .col(
                        ColumnDef::new(PlansFlexible::Steps)
                            .string()
                            .not_null()
                            .default("[]"),
                    )
                    .col(
                        ColumnDef::new(PlansFlexible::ExecutionPlan)
                            .string()
                            .not_null()
                            .default("[]"),
                    )
                    .col(ColumnDef::new(PlansFlexible::CreatedAt).string().not_null())
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
            .drop_table(Table::drop().table(PlansFlexible::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Plans {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum PlansFlexible {
    Table,
    Id,
    PlanId,
    Version,
    InputSchema,
    Output,
    Steps,
    ExecutionPlan,
    CreatedAt,
}
