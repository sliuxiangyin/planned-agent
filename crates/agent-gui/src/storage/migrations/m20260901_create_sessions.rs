//! sessions 表迁移 — 灵活模式「会话（创作过程）」生命周期。
//!
//! 每个 session 对应一段「发起意图 → 澄清 → 执行 → 定稿」的生产过程，
//! 记录其状态（进行中/已封版/被弃）、衍生来源版本与参考注入上下文。
//! chat_messages / plans_flexible 通过 session_id 关联到本表。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Sessions::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Sessions::PlanId).string().not_null())
                    .col(ColumnDef::new(Sessions::Status).string().not_null())
                    .col(
                        ColumnDef::new(Sessions::DerivedFromVersion)
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Sessions::ReferenceContext).string().null())
                    .col(ColumnDef::new(Sessions::CreatedAt).string().not_null())
                    .col(ColumnDef::new(Sessions::UpdatedAt).string().not_null())
                    .col(ColumnDef::new(Sessions::ClosedAt).string().null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_sessions_plan_id")
                            .from(Sessions::Table, Sessions::PlanId)
                            .to(Plans::Table, Plans::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_sessions_plan")
                    .table(Sessions::Table)
                    .col(Sessions::PlanId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Sessions::Table).to_owned())
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
enum Sessions {
    Table,
    Id,
    PlanId,
    Status,
    DerivedFromVersion,
    ReferenceContext,
    CreatedAt,
    UpdatedAt,
    ClosedAt,
}
