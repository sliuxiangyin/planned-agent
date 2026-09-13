//! flexible_step2 结果回调：解析子 agent 输出，并把「执行成功」定稿登记到流程状态。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`
//! （见 `docs/chat-flexible-回调会话归属设计.md`），据此判断本次执行属于哪个会话。
//!
//! 定稿判定（与 `flexible_step2.toml` 的输出契约一致）：
//! - `{"status":"success", ...}` → 登记 `executed`（写入 `execution_trace` + `compressed_context`），
//!   并清除下游产物（重跑 step2 等于放弃 step3/step4 已定稿的内容）。
//! - 其它（`status:"error"` / 非 JSON）→ **不登记**，保持原阶段（协调器按 prompt 询问重试或取消）。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{ResultDecision, SubAgentCallContext, SubAgentResultCallback};
use planned_agent_core::mcp::types::ToolResult;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::{read_host_session_id, HOST_SESSION_ID_FIELD};

/// flexible_step2 结果回调。
///
/// 子 agent 完成后 `on_result` 被调用，`result.content` 为
/// `extract_last_assistant_text` 的文本（即子 agent 最终输出，按契约是纯 JSON）。
pub struct FlexibleStep2Callback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（读/写流程中间状态）。
    service: Arc<PlansFlexibleService>,
}

#[async_trait]
impl SubAgentResultCallback for FlexibleStep2Callback {
    async fn on_result(&self, ctx: &SubAgentCallContext, result: &ToolResult) -> ResultDecision {
        let text = result.content.as_str().unwrap_or("");
        tracing::info!(
            "[flexible_step2] 子 agent '{}' 完成, tool_call_id={}, content_len={}, is_error={}",
            ctx.agent_name,
            ctx.tool_call_id,
            text.len(),
            result.is_error,
        );

        // ── 会话归属：值来自父 agent 传入的原始参数（核心库只透传，语义由这里定义）──
        let Some(session_id) = read_host_session_id(&ctx.arguments) else {
            tracing::warn!(
                "[flexible_step2] 回调未拿到 {}（父 agent 是否按 schema 传参？），跳过状态登记",
                HOST_SESSION_ID_FIELD
            );
            return ResultDecision::Accept;
        };

        // ── 定稿判定：step2 契约约定输出为纯 JSON ──
        let parsed: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("[flexible_step2] 输出非合法 JSON，跳过状态登记: {}", e);
                return ResultDecision::Accept;
            }
        };
        if parsed.get("status").and_then(serde_json::Value::as_str) != Some("success") {
            // 失败/未知状态：本次未产生有效产物，**不推进** current_step。
            tracing::warn!(
                "[flexible_step2] 执行未成功（status={:?}），不登记产物",
                parsed.get("status").and_then(serde_json::Value::as_str)
            );
            return ResultDecision::Accept;
        }

        // ── 登记 executed，并清除下游（重跑 step2 ⇒ step3/step4 的定稿产物作废）──
        let mut patch = serde_json::Map::new();
        // 产物**缺失即跳过写入**（保留原值）——只有显式提供才写入。
        // 否则 `merge_state` 会把 `null` 当「删除」，在 step2 漏字段时静默清掉已有产物。
        if let Some(trace) = parsed.get("execution_trace").filter(|v| !v.is_null()) {
            patch.insert("execution_trace".to_string(), trace.clone());
        } else {
            tracing::warn!("[flexible_step2] success 但缺 execution_trace，跳过该产物写入");
        }
        if let Some(ctx_summary) = parsed.get("compressed_context").filter(|v| !v.is_null()) {
            patch.insert("compressed_context".to_string(), ctx_summary.clone());
        } else {
            tracing::warn!("[flexible_step2] success 但缺 compressed_context，跳过该产物写入");
        }
        patch.insert("field_selection_result".to_string(), serde_json::Value::Null);
        patch.insert("parameter_confirmation_result".to_string(), serde_json::Value::Null);

        match self
            .service
            .merge_state(&self.plan_id, &session_id, Some("executed"), &patch)
            .await
        {
            Ok((step, _)) => tracing::info!(
                "[flexible_step2] 状态已登记: plan_id={}, host_session_id={}, current_step={}",
                self.plan_id,
                session_id,
                step,
            ),
            Err(e) => tracing::error!(
                "[flexible_step2] 登记状态失败: plan_id={}, host_session_id={}, err={}",
                self.plan_id,
                session_id,
                e,
            ),
        }

        ResultDecision::Accept
    }
}

/// 创建 `flexible_step2` 回调实例（方便传给 `register_sub_agent`）。
pub fn create_step2_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Option<Arc<dyn SubAgentResultCallback>> {
    Some(Arc::new(FlexibleStep2Callback { plan_id, service }))
}
