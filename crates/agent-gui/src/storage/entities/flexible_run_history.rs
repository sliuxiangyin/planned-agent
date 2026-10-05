//! flexible_run_history 表 entity — 每步「上次成功用过的工具链」记忆。
//!
//! 一步一行，由 `(session_id, result_reference)` 唯一；`revision` 是判据列（写这行时
//! `plans_flexible_sessions.revision` 的值），注入时只认「当前这一代」的行。
//!
//! 与 `plans_flexible_sessions`（定稿快照）语义分离：本表存**执行事实**（工具路径），
//! 且**不重复存 `steps`**（其权威来源是 `parameterized_task`）。

use sea_orm::entity::prelude::*;

/// flexible_run_history 表主键 id（UUID 字符串）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "flexible_run_history")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// FK → plans.id
    pub plan_id: String,
    /// 归属会话/版本 id（FK → plans_flexible_sessions.id）
    pub session_id: String,
    /// 写这一行时的 `plans_flexible_sessions.revision`（判据列，不参与唯一性）
    #[sea_orm(default_value = 0)]
    pub revision: i32,
    /// 步骤标识（`#E1`），与 `parameterized_task.steps[].result_reference` 对齐
    pub result_reference: String,
    /// **反参数化后**的工具调用路径（JSON 数组），缺省 `[]`
    #[sea_orm(default_value = "[]")]
    pub tool_chain: String,
    pub created_at: String,
    pub updated_at: String,
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
