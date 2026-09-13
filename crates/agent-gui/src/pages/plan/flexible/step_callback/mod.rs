//! step 子 agent 的完成回调（`SubAgentResultCallback`）与共享工具。
//!
//! 每个 `flexible_stepN` 子 agent 在完成后把「定稿」登记到流程状态，各自一个子模块
//! （`step1_callback` ~ `step5_callback`）；它们的差异只有 [`step_commit::StepSpec`]
//! 里的契约，通用流程见 [`step_commit`]。多个回调共用的「会话归属」读取工具放在本文件。
//!
//! 设计背景见 `docs/chat-flexible-回调会话归属设计.md`。

pub(crate) mod step1_callback;
pub(crate) mod step2_callback;
pub(crate) mod step3_callback;
pub(crate) mod step4_callback;
pub(crate) mod step5_callback;
pub(crate) mod step_commit;

pub(crate) use step1_callback::create_step1_callback;
pub(crate) use step2_callback::create_step2_callback;
pub(crate) use step3_callback::create_step3_callback;
pub(crate) use step4_callback::create_step4_callback;
pub(crate) use step5_callback::create_step5_callback;

/// 子 agent 调用参数中承载「宿主会话 id」的字段名。
///
/// 见 `docs/chat-flexible-回调会话归属设计.md` §5：该字段由父协调器把 system prompt
/// 「会话上下文」里的 session_id **原样照抄**进 step 子 agent 的调用参数，供子 agent
/// 完成回调（`SubAgentResultCallback`）定位「本次执行属于哪个会话」。
///
/// 注意：不要与子 agent 挂起-恢复用的 `session_id`（`SubAgentSessionStore` 的内存键）
/// 混用 —— 那个是运行期句柄，这个是宿主会话（落库归属）。
pub(crate) const HOST_SESSION_ID_FIELD: &str = "host_session_id";

/// 从子 agent 调用参数里读取宿主会话 id（字段缺失或类型不符时返回 `None`）。
pub(crate) fn read_host_session_id(args: &serde_json::Value) -> Option<String> {
    args.get(HOST_SESSION_ID_FIELD)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}
