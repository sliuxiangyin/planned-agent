//! flexible_step5 结果回调：模板编译成功后把状态推进到 `templated`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`。
//!
//! 定稿判定（与 `flexible_step5.toml` 的输出契约一致）：
//! - `{"status":"success", ...}` → 只推进 `current_step = "templated"`，**不写任何 `products`**。
//! - 其它（`status:"error"` / 非 JSON）→ **不登记**。
//!
//! 职责边界：本回调**不做**模板落库。`plans_flexible_sessions` 的写入（含
//! `steps` / `execution_plan` 结构校验）仍由 `flexible_save_template` 工具负责——只有工具侧
//! 才知道落库是否成功。回调只承担「状态推进 / 定稿记录」。
//!
//! 通用逻辑见 [`super::step_commit`]。

use std::sync::Arc;

use planned_agent::chat::SubAgentResultCallback;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::step_commit::{StepCallback, StepSpec};

/// flexible_step5 的定稿契约：只推进档位，不写 `products`。
const SPEC: StepSpec = StepSpec {
    agent: "flexible_step5",
    ok_status: "success",
    next_step: "templated",
    products: &[],
    clear: &[],
};

/// 创建 `flexible_step5` 回调实例（方便传给 `register_sub_agent`）。
pub fn create_step5_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Option<Arc<dyn SubAgentResultCallback>> {
    Some(Arc::new(StepCallback::new(plan_id, service, SPEC)))
}
