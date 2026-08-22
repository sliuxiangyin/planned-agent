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
    /// 灵活模式当前使用的版本号，0 = 尚未生成（周密模式忽略此字段）
    #[sea_orm(default_value = 0)]
    pub flexible_version: i32,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
