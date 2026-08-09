//! plans_flexible 表迁移：添加 input_schema 列。
//!
//! 计划执行所需的外部输入参数定义（JSON 文本）。计划保存后供下次执行
//! 动态替换参数、以及多计划工作流的输入对接。

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
                        ColumnDef::new(PlansFlexible::InputSchema)
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
                    .drop_column(PlansFlexible::InputSchema)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum PlansFlexible {
    Table,
    InputSchema,
}
