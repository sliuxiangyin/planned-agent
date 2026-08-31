//! chat_messages 表 entity — 灵活模式聊天消息持久化。

use sea_orm::entity::prelude::*;

/// chat_messages 表主键 id（UUID 字符串）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "chat_messages")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 关联计划 id
    pub plan_id: String,
    /// Message 的完整 JSON 序列化
    pub message_json: String,
    /// 显示排序序号
    pub sequence_order: i32,
    /// 错误类型：0=无错误，1=工具执行错误，2=中断错误
    #[sea_orm(default_value = 0)]
    pub is_error_type: i32,
    /// 是否为子 agent 工具调用
    #[sea_orm(default_value = false)]
    pub is_agent_tool: bool,
    pub created_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
