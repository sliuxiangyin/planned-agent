//! plans_flexible_sessions 表迁移 —— 新增 `revision`（会话内计划修订号）。
//!
//! 背景：`flexible_run_history`（同批新增）要按「计划的第几代 steps」隔离工具链记忆。
//! `revision` 由 `produce` 在**检测到 `steps` 内容变化**时 +1（比较逻辑在宿主 service 层，
//! 只比 `steps`，全字段含顺序）；未变化则不动 —— 因此只改 `inputs` / `output_schema`
//! 不算换版，记忆得以保留。
//!
//! 老数据该列默认 0；首次 `produce` 后变为 1。
//! 与 `version` 的区别：`version` 是「会话即版本」的语义化版本号（新建会话时分配），
//! `revision` 是**同一会话内**计划内容的代数。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PlansFlexibleSessions::Table)
                    .add_column(
                        ColumnDef::new(PlansFlexibleSessions::Revision)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PlansFlexibleSessions::Table)
                    .drop_column(PlansFlexibleSessions::Revision)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum PlansFlexibleSessions {
    Table,
    Revision,
}
