//! plans_flexible 表 entity — 灵活模式计划的版本快照。

use sea_orm::entity::prelude::*;

/// plans_flexible 表主键 id（UUID 字符串）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "plans_flexible")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// FK → plans.id
    pub plan_id: String,
    /// 版本号：1, 2, 3...
    pub version: i32,
    /// AI 自然语言总结原文
    pub previous_summary: String,
    /// CoarseGrainedPlan JSON
    #[sea_orm(default_value = "[]")]
    pub todos: String,
    /// 计划参数 JSON（ParamDef 数组，来自清晰度检查固化勾选）
    #[sea_orm(default_value = "[]")]
    pub params: String,
    /// 输出格式描述（占位预留：多计划关联执行时描述本计划产出格式，当前未实现读写逻辑）
    #[sea_orm(default_value = "")]
    pub output_schema: String,
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
