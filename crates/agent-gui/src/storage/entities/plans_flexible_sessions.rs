//! plans_flexible_sessions 表 entity — 「会话即版本」合并表。
//! 一个会话即一个版本；定稿产物四件套仅在定稿（produced）时写入，未定稿为 NULL。

use sea_orm::entity::prelude::*;

/// plans_flexible_sessions 表主键 id（UUID 字符串）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "plans_flexible_sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// FK → plans.id
    pub plan_id: String,
    /// 会话标题
    #[sea_orm(default_value = "")]
    pub title: String,
    /// 语义化版本号 vX.Y.Z；plan 内唯一、单调递增
    pub version: String,
    /// 会话状态：active / produced / abandoned
    #[sea_orm(default_value = "active")]
    pub status: String,
    /// 是否为默认计划
    #[sea_orm(default_value = false)]
    pub is_default: bool,
    /// 输入参数定义 JSON（定稿产物，可空）
    pub input_schema: Option<String>,
    /// 输出定义 JSON（定稿产物，可空）
    pub output: Option<String>,
    /// 硬编码执行脚本 JSON 数组（定稿产物，可空）
    pub steps: Option<String>,
    /// 动态修复说明书 JSON 数组（定稿产物，可空）
    pub execution_plan: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
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
