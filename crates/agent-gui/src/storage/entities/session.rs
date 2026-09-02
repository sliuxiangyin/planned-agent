//! sessions 表 entity — 灵活模式「会话（创作过程）」生命周期。
//!
//! 每个 session 对应一次「发起意图 → 澄清 → 执行 → 定稿」的生产过程：
//! - `status`：`active`（进行中/未定稿）、`produced`（已定稿封版）、`abandoned`（中途被弃）。
//! - `derived_from_version`：本会话从哪个成果版本衍生（首会话为 null）。
//! - `reference_context`：开此会话时注入的上次成果参考（首会话为 null）。

use sea_orm::entity::prelude::*;

/// sessions 表主键 id（UUID 字符串，即 session_id）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 关联计划 id（FK → plans.id）
    pub plan_id: String,
    /// 会话状态：active / produced / abandoned
    pub status: String,
    /// 本会话从哪个版本衍生；首会话为 null
    pub derived_from_version: Option<i32>,
    /// 开此会话时注入的上次成果参考文本
    pub reference_context: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// 封版时间（produced/abandoned 时写入）
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
