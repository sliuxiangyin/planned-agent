//! flexible_step3 结果回调：字段选择定稿后登记 `fields_selected`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`。
//! step3 几乎必然挂起（要请用户勾选输出字段），挂起-恢复路径同样会带 `arguments`
//! （见 `docs/chat-flexible-回调会话归属设计.md` §8），故恢复后回调仍能定位会话。
//!
//! 定稿判定（与 `flexible_step3.toml` 的输出契约一致）：
//! - `{"status":"fields_selected", ...}` → 登记 `fields_selected`（写入 `field_selection_result`），
//!   并清除下游产物 `parameter_confirmation_result`。
//! - 其它（`empty_result` / `back_to_execute` / `cancelled` / 非 JSON）→ **不登记**。
//!
//! 通用逻辑见 [`super::step_commit`]。

use std::sync::Arc;

use planned_agent::chat::SubAgentResultCallback;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::step_commit::{StepCallback, StepSpec};

/// flexible_step3 的定稿契约。
const SPEC: StepSpec = StepSpec {
    agent: "flexible_step3",
    ok_status: "fields_selected",
    next_step: "fields_selected",
    products: &["field_selection_result"],
    payload_key: None,
    clear: &["parameter_confirmation_result"],
};

/// 创建 `flexible_step3` 回调链（方便传给 `register_sub_agent`）。
///
/// 返回 `Vec`：结果回调支持多实现串行（见 `collect::run_chain`），本 step 目前只挂一个。
pub fn create_step3_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Vec<Arc<dyn SubAgentResultCallback>> {
    vec![Arc::new(StepCallback::new(plan_id, service, SPEC))]
}
