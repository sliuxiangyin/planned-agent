//! flexible_step4 结果回调：参数确认定稿后登记 `params_confirmed`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`。
//! 与 step3 同理，step4 请用户勾选参数时也会挂起，恢复路径带 `arguments`。
//!
//! 定稿判定（与 `flexible_step4.toml` 的输出契约一致）：
//! - `{"status":"params_confirmed", ...}` → 登记 `params_confirmed`（写入 `parameter_confirmation_result`）。
//! - 其它（`back_to_step3` / `cancelled` / 非 JSON）→ **不登记**。
//!
//! 本档位已是最高的执行前档位，下游只剩 step5 的模板落库（写 `plans_flexible`，不入 `products`），
//! 故没有需要清除的下游产物。
//!
//! 通用逻辑见 [`super::step_commit`]。

use std::sync::Arc;

use planned_agent::chat::SubAgentResultCallback;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::step_commit::{StepCallback, StepSpec};

/// flexible_step4 的定稿契约。
const SPEC: StepSpec = StepSpec {
    agent: "flexible_step4",
    ok_status: "params_confirmed",
    next_step: "params_confirmed",
    products: &["parameter_confirmation_result"],
    payload_key: None,
    clear: &[],
};

/// 创建 `flexible_step4` 回调链（方便传给 `register_sub_agent`）。
///
/// 返回 `Vec`：结果回调支持多实现串行（见 `collect::run_chain`），本 step 目前只挂一个。
pub fn create_step4_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Vec<Arc<dyn SubAgentResultCallback>> {
    vec![Arc::new(StepCallback::new(plan_id, service, SPEC))]
}
