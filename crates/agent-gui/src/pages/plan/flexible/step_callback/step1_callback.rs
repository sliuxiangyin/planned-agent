//! flexible_step1 结果回调：需求澄清定稿后登记 `task_defined`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`。
//!
//! 定稿判定（与 `flexible_step1.toml` 的输出契约一致）：
//! - `{"status":"task_defined", ...}` → 登记 `task_defined`（写入 `task_definition` + `output_format`），
//!   并清除 step2~step4 的全部下游产物。
//! - 其它（`status:"ignored"` / `"cancelled"` / 非 JSON）→ **不登记**。
//!
//! 语义注记：本回调登记的是「子 agent 已把需求澄清成一份任务定义」这个**定稿动作**，
//! 不涉及「用户是否认同该需求」——用户不认同就会继续补充 / 修改，协调器重跑 step1 后
//! 回调再登记一次、覆盖旧值，二者不冲突。
//!
//! 通用逻辑见 [`super::step_commit`]。

use std::sync::Arc;

use planned_agent::chat::SubAgentResultCallback;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::step_commit::{StepCallback, StepSpec};

/// flexible_step1 的定稿契约。
const SPEC: StepSpec = StepSpec {
    agent: "flexible_step1",
    ok_status: "task_defined",
    next_step: "task_defined",
    products: &["task_definition", "output_format"],
    clear: &[
        "execution_trace",
        "compressed_context",
        "field_selection_result",
        "parameter_confirmation_result",
    ],
};

/// 创建 `flexible_step1` 回调实例（方便传给 `register_sub_agent`）。
pub fn create_step1_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Option<Arc<dyn SubAgentResultCallback>> {
    Some(Arc::new(StepCallback::new(plan_id, service, SPEC)))
}
