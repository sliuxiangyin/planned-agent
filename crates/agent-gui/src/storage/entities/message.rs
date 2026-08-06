//! messages 表 entity — 对话消息持久化。

use sea_orm::entity::prelude::*;

/// messages 表主键 id（UUID 字符串）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "messages")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 关联计划 id
    pub plan_id: String,
    /// 角色：user / assistant / system / tool
    pub role: String,
    #[sea_orm(default_value = "")]
    pub content: String,
    pub created_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::plan::Entity",
        from = "Column::PlanId",
        to = "super::plan::Column::Id"
    )]
    Plan,
}

impl Related<super::plan::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Plan.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
