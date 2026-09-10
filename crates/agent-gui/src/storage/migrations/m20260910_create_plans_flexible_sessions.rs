//! plans_flexible_sessions 表迁移 — 「会话即版本」合并表。
//!
//! 设计背景：原 `sessions`（创作过程）与 `plans_flexible`（定稿模板版本快照）
//! 语义重叠 —— plans_flexible 一个会话至多一条、version 只是会话的封版序号投影，
//! 故合并为单表 plans_flexible_sessions。
//!
//! 语义要点：
//! - 每个会话 = 一个版本；新建会话即新建版本。
//! - `version` 为语义化版本号（如 v1.0.0），唯一（plan 内不可重复）、只增不减，
//!   分配逻辑由 repo 层保证单调递增。
//! - `status` 表达定稿态：`active`（进行中/未定稿）→ `produced`（已定稿可回看/可执行）/
//!   `abandoned`（被弃）。
//! - `is_default` 标记该会话是否为 plan 的默认会话。
//! - 定稿产物四件套（input_schema / output / steps / execution_plan）为**可空列**，
//!   仅在 `status → produced` 定稿时写入；未定稿会话这几列为 NULL。
//! - `title` 为该版本会话标题。
//!
//! 该表为 plans_flexible_sessions 会话体系的新归属，替代旧 sessions / plans_flexible 两表。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(PlansFlexibleSessions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PlansFlexibleSessions::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(PlansFlexibleSessions::PlanId)
                            .string()
                            .not_null(),
                    )
                    // 会话标题
                    .col(
                        ColumnDef::new(PlansFlexibleSessions::Title)
                            .string()
                            .not_null()
                            .default(""),
                    )
                    // 语义化版本号 vX.Y.Z；plan 内唯一、单调递增
                    .col(
                        ColumnDef::new(PlansFlexibleSessions::Version)
                            .string()
                            .not_null(),
                    )
                    // 会话状态：active / produced / abandoned
                    .col(
                        ColumnDef::new(PlansFlexibleSessions::Status)
                            .string()
                            .not_null()
                            .default("active"),
                    )
                    // 是否为默认计划
                    .col(
                        ColumnDef::new(PlansFlexibleSessions::IsDefault)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    // 定稿产物四件套（可空，produced 时写入）
                    .col(ColumnDef::new(PlansFlexibleSessions::InputSchema).string().null())
                    .col(ColumnDef::new(PlansFlexibleSessions::Output).string().null())
                    .col(ColumnDef::new(PlansFlexibleSessions::Steps).string().null())
                    .col(
                        ColumnDef::new(PlansFlexibleSessions::ExecutionPlan)
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(PlansFlexibleSessions::CreatedAt)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PlansFlexibleSessions::UpdatedAt)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(PlansFlexibleSessions::ClosedAt)
                            .string()
                            .null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_plans_flexible_sessions_plan_id")
                            .from(
                                PlansFlexibleSessions::Table,
                                PlansFlexibleSessions::PlanId,
                            )
                            .to(Plans::Table, Plans::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // plan 内版本号唯一：同一 plan 下 version 不可重复
        manager
            .create_index(
                Index::create()
                    .name("idx_plans_flexible_sessions_plan_version")
                    .table(PlansFlexibleSessions::Table)
                    .col(PlansFlexibleSessions::PlanId)
                    .col(PlansFlexibleSessions::Version)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // 按 plan 检索会话/版本列表
        manager
            .create_index(
                Index::create()
                    .name("idx_plans_flexible_sessions_plan")
                    .table(PlansFlexibleSessions::Table)
                    .col(PlansFlexibleSessions::PlanId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PlansFlexibleSessions::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// 复用同文件中 Plans 的列标识（仅引用 FK 目标表名）
#[derive(DeriveIden)]
enum Plans {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum PlansFlexibleSessions {
    Table,
    Id,
    PlanId,
    Title,
    Version,
    Status,
    IsDefault,
    InputSchema,
    Output,
    Steps,
    ExecutionPlan,
    CreatedAt,
    UpdatedAt,
    ClosedAt,
}
