//! plans_flexible 表迁移：添加 output_schema 占位列。
//!
//! 多计划关联执行时用于描述本计划的产出格式，当前仅占位，无业务读写逻辑。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(PlansFlexible::Table)
                    .add_column(
                        ColumnDef::new(PlansFlexible::OutputSchema)
                            .string()
                            .not_null()
                            .default(""),
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
                    .table(PlansFlexible::Table)
                    .drop_column(PlansFlexible::OutputSchema)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum PlansFlexible {
    Table,
    OutputSchema,
}
