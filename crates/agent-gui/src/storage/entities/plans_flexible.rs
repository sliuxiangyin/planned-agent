//! plans_flexible 表 entity — 灵活模式计划的版本快照。
//! 存储 flexible_step5 生成的混合模板（input_schema / output / steps / execution_plan），
//! 其中 metadata 拆为 version + created_at 两列。

use sea_orm::entity::prelude::*;

/// plans_flexible 表主键 id（UUID 字符串）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "plans_flexible")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// FK → plans.id
    pub plan_id: String,
    /// 版本号：1, 2, 3...（对应 step5 输出 metadata.version）
    pub version: i32,
    /// 输入参数定义 JSON（来自 step5 输出 input_schema）
    #[sea_orm(default_value = "{}")]
    pub input_schema: String,
    /// 输出定义 JSON（来自 step5 输出 output：format + fields）
    #[sea_orm(default_value = "{}")]
    pub output: String,
    /// 硬编码执行脚本 JSON 数组（来自 step5 输出 steps）
    #[sea_orm(default_value = "[]")]
    pub steps: String,
    /// 动态修复说明书 JSON 数组（来自 step5 输出 execution_plan）
    #[sea_orm(default_value = "[]")]
    pub execution_plan: String,
    /// 创建时间（对应 step5 输出 metadata.created_at）
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
