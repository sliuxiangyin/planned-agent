//! flexible_state 表 entity — 灵活模式「流程中间状态槽」。
//!
//! 按 plan+session 记录一次灵活流程推进到的当前阶段（`current_step`）与各步骤产物
//! （`products`）。用于协调器判定「用户指定步骤」时前置产物是否齐备：
//! - `current_step`：`none`→`task_defined`→`executed`→`fields_selected`→
//!   `params_confirmed`→`templated`，与协调器状态机阶段一一对应；
//! - `products`：JSON 对象，键为 `task_definition / output_format / execution_trace /
//!   compressed_context / field_selection_result / parameter_confirmation_result`。
//!
//! 与 `plans_flexible`（step5 定稿快照）语义分离：本表存过程进度与中间产物，
//! 后者存最终模板版本。

use sea_orm::entity::prelude::*;

/// flexible_state 表主键 id（UUID 字符串）
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "flexible_state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// FK → plans.id
    pub plan_id: String,
    /// 归属会话 id（FK → sessions.id）；状态必然归属某会话，非空
    pub session_id: String,
    /// 流程推进到的当前阶段（none/task_defined/executed/fields_selected/params_confirmed/templated）
    #[sea_orm(default_value = "none")]
    pub current_step: String,
    /// 各步骤中间产物 JSON 对象
    #[sea_orm(default_value = "{}")]
    pub products: String,
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
