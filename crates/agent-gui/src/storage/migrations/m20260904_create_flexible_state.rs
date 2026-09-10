//! flexible_state 表迁移（存储灵活模式流程中间状态：当前阶段 + 各步骤产物）。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(FlexibleState::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FlexibleState::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FlexibleState::PlanId).string().not_null())
                    // 状态必然归属某会话（plans_flexible_sessions 的会话/版本），非空
                    .col(ColumnDef::new(FlexibleState::SessionId).string().not_null())
                    .col(
                        ColumnDef::new(FlexibleState::CurrentStep)
                            .string()
                            .not_null()
                            .default("none"),
                    )
                    .col(
                        ColumnDef::new(FlexibleState::Products)
                            .string()
                            .not_null()
                            .default("{}"),
                    )
                    .col(ColumnDef::new(FlexibleState::CreatedAt).string().not_null())
                    .col(ColumnDef::new(FlexibleState::UpdatedAt).string().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_flexible_state_plan_id")
                            .from(FlexibleState::Table, FlexibleState::PlanId)
                            .to(Plans::Table, Plans::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_flexible_state_session_id")
                            .from(FlexibleState::Table, FlexibleState::SessionId)
                            .to(PlansFlexibleSessions::Table, PlansFlexibleSessions::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_flexible_state_plan_session")
                    .table(FlexibleState::Table)
                    .col(FlexibleState::PlanId)
                    .col(FlexibleState::SessionId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(FlexibleState::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Plans {
    Table,
    Id,
}

/// plans_flexible_sessions 表名引用（FK 目标）
#[derive(DeriveIden)]
enum PlansFlexibleSessions {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum FlexibleState {
    Table,
    Id,
    PlanId,
    SessionId,
    CurrentStep,
    Products,
    CreatedAt,
    UpdatedAt,
}
