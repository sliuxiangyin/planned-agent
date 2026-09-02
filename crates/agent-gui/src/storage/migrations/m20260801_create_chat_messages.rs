//! chat_messages 表迁移 — 灵活模式聊天消息持久化。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ChatMessages::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ChatMessages::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ChatMessages::PlanId).string().not_null())
                    // session_id 关联归属会话；会话生命周期尚未接入前可为空
                    .col(ColumnDef::new(ChatMessages::SessionId).string().null())
                    .col(ColumnDef::new(ChatMessages::MessageJson).string().not_null())
                    .col(
                        ColumnDef::new(ChatMessages::SequenceOrder)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ChatMessages::IsErrorType)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(ChatMessages::IsAgentTool)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(ChatMessages::CreatedAt).string().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_chat_messages_plan_id")
                            .from(ChatMessages::Table, ChatMessages::PlanId)
                            .to(Plans::Table, Plans::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_chat_messages_session_id")
                            .from(ChatMessages::Table, ChatMessages::SessionId)
                            .to(Sessions::Table, Sessions::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_chat_messages_plan")
                    .table(ChatMessages::Table)
                    .col(ChatMessages::PlanId)
                    .col(ChatMessages::SequenceOrder)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_chat_messages_session")
                    .table(ChatMessages::Table)
                    .col(ChatMessages::SessionId)
                    .col(ChatMessages::SequenceOrder)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ChatMessages::Table).to_owned())
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

/// sessions 表名引用（FK 目标）
#[derive(DeriveIden)]
enum Sessions {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum ChatMessages {
    Table,
    Id,
    PlanId,
    SessionId,
    MessageJson,
    SequenceOrder,
    IsErrorType,
    IsAgentTool,
    CreatedAt,
}
