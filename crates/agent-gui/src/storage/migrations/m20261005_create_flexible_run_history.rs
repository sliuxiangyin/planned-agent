//! flexible_run_history 表迁移 —— 每步「上次成功用过的工具链」记忆。
//!
//! 语义要点：
//! - **一步一行**：由 `(session_id, result_reference)` 唯一，重复执行覆盖（upsert）。
//! - `revision` 是**判据列**：写入该行时 `plans_flexible_sessions.revision` 的值。
//!   注入时只认「行内 revision == 会话当前 revision」的行 —— `steps` 内容换过一代，
//!   旧行自动失配、一条都不注入（由新一次执行覆盖），因此**不需要清理策略**。
//! - `tool_chain` 为**反参数化后**的工具调用路径（JSON 数组，形如
//!   `[{"tool":"read_file","args_shape":"{\"path\":\"${file_path}\"}"}]`）；
//!   占位符在注入时用本次 params 重新展开，避免照抄上次的具体取值。
//! - 本表**不存 `steps` 的副本**：`steps` 的唯一权威来源是
//!   `plans_flexible_sessions.parameterized_task`；需要完整视图时按 `result_reference`
//!   把两边合并（合成结果 = steps json 每项多一个 `tool_chain` 字段）。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(FlexibleRunHistory::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FlexibleRunHistory::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FlexibleRunHistory::PlanId).string().not_null())
                    // 归属会话（plans_flexible_sessions 的会话/版本 id），非空
                    .col(ColumnDef::new(FlexibleRunHistory::SessionId).string().not_null())
                    // 判据列：写这一行时的 plans_flexible_sessions.revision
                    .col(
                        ColumnDef::new(FlexibleRunHistory::Revision)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    // 步骤标识（#E1），与 parameterized_task.steps[].result_reference 对齐
                    .col(
                        ColumnDef::new(FlexibleRunHistory::ResultReference)
                            .string()
                            .not_null(),
                    )
                    // 反参数化后的工具调用路径（JSON 数组）
                    .col(
                        ColumnDef::new(FlexibleRunHistory::ToolChain)
                            .string()
                            .not_null()
                            .default("[]"),
                    )
                    .col(ColumnDef::new(FlexibleRunHistory::CreatedAt).string().not_null())
                    .col(ColumnDef::new(FlexibleRunHistory::UpdatedAt).string().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_flexible_run_history_plan_id")
                            .from(FlexibleRunHistory::Table, FlexibleRunHistory::PlanId)
                            .to(Plans::Table, Plans::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_flexible_run_history_session_id")
                            .from(FlexibleRunHistory::Table, FlexibleRunHistory::SessionId)
                            .to(PlansFlexibleSessions::Table, PlansFlexibleSessions::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // 一步一行：同一会话同一步骤只留最新一份（upsert 的地基）
        manager
            .create_index(
                Index::create()
                    .name("idx_flexible_run_history_session_reference")
                    .table(FlexibleRunHistory::Table)
                    .col(FlexibleRunHistory::SessionId)
                    .col(FlexibleRunHistory::ResultReference)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // 注入时按 (session_id, revision) 取「当前这一代」的行
        manager
            .create_index(
                Index::create()
                    .name("idx_flexible_run_history_session_revision")
                    .table(FlexibleRunHistory::Table)
                    .col(FlexibleRunHistory::SessionId)
                    .col(FlexibleRunHistory::Revision)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(FlexibleRunHistory::Table).to_owned())
            .await?;
        Ok(())
    }
}

/// plans 表名引用（FK 目标）
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
enum FlexibleRunHistory {
    Table,
    Id,
    PlanId,
    SessionId,
    Revision,
    ResultReference,
    ToolChain,
    CreatedAt,
    UpdatedAt,
}
