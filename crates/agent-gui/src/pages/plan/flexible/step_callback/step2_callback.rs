//! flexible_step2 结果回调：执行成功后登记 `executed`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`
//! （见 `docs/chat-flexible-回调会话归属设计.md`），据此判断本次执行属于哪个会话。
//!
//! 定稿判定（与 `flexible_step2.toml` 的输出契约一致）：
//! - `{"status":"success", ...}` → 登记 `executed`（写入 `execution_trace` + `compressed_context`），
//!   并清除下游产物（重跑 step2 等于放弃 step3/step4 已定稿的内容）。
//! - 其它（`status:"error"` / 非 JSON）→ **不登记**，保持原阶段（协调器按 prompt 询问重试或取消）。
//!
//! 通用逻辑见 [`super::step_commit`]。

use std::sync::Arc;

use planned_agent::chat::SubAgentResultCallback;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::step_commit::{StepCallback, StepSpec};

/// flexible_step2 的定稿契约。
const SPEC: StepSpec = StepSpec {
    agent: "flexible_step2",
    ok_status: "success",
    next_step: "executed",
    products: &["execution_trace", "compressed_context"],
    payload_key: None,
    clear: &["field_selection_result", "parameter_confirmation_result"],
};

/// 创建 `flexible_step2` 回调链（方便传给 `register_sub_agent`）。
///
/// 返回 `Vec`：结果回调支持多实现串行（见 `collect::run_chain`），本 step 目前只挂一个。
pub fn create_step2_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Vec<Arc<dyn SubAgentResultCallback>> {
    vec![Arc::new(StepCallback::new(plan_id, service, SPEC))]
}
