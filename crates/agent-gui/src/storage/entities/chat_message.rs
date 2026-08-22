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
    /// 消息类型：user / text / reasoning / tool_call / tool_result
    pub msg_type: String,
    /// 工具执行是否失败（仅 tool_result 有意义）
    #[sea_orm(default_value = false)]
    pub is_error: bool,
    pub created_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
