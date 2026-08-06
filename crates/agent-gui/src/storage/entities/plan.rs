//! plans 表 entity — 计划元数据持久化。

use sea_orm::entity::prelude::*;

/// plans 表主键 id（UUID 字符串）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "plans")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub name: String,
    #[sea_orm(default_value = "")]
    pub description: String,
    /// 计划模式：flexible / thorough
    pub mode: String,
    /// 状态：pending_generation / generated
    pub status: String,
    /// JSON 数组，存储 todo 条目（预留）
    #[sea_orm(default_value = "[]")]
    pub todos: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::message::Entity")]
    Messages,
}

impl Related<super::message::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Messages.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
