//! `flexible_save` 的定稿登记回调：保存定稿后把 `parameterized_task` 落库并推进 `saved`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`。
//!
//! 定稿判定（与 `flexible_save.toml` 的输出契约一致）：`{"status":"saved", ...}` 才算定稿；
//! 其它（`status:"error"` / 非 JSON）**不登记、不落库**。
//!
//! 职责边界：本回调**直接落库** —— 读 `flexible_state.products.parameterized_task`
//! 整段写入 `plans_flexible_sessions`，不经协调器 LLM 转抄（转抄会改坏字段）。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{ResultDecision, SubAgentCall, SubAgentResultCallback};
use serde_json::{Map, Value};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::super::analysis::require_analysis;
use super::super::commit::{commit_state, hand_off};

/// 子 agent 工具名（日志用，链组装也要用）。
pub(super) const AGENT: &str = "flexible_save";
/// 定稿 status：输出顶层 `status` 等于它才算定稿。
pub(super) const OK_STATUS: &str = "saved";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "saved";

/// `flexible_save` 的定稿登记回调。
pub(super) struct SaveCallback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态 + 落库）。
    service: Arc<PlansFlexibleService>,
}

impl SaveCallback {
    pub(super) fn new(plan_id: String, service: Arc<PlansFlexibleService>) -> Self {
        Self { plan_id, service }
    }
}

/// 从 `flexible_state.products`（JSON 文本）取出 `parameterized_task`（须为对象），返回其 JSON 字符串。
fn extract_parameterized_task(products: &str) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(products)
        .map_err(|e| format!("products 非法 JSON: {e}"))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| "products 不是对象".to_string())?;
    let Some(pt) = obj.get("parameterized_task").filter(|v| !v.is_null()) else {
        return Err("state 缺 parameterized_task（step2 尚未定稿）".to_string());
    };
    if !pt.is_object() {
        return Err("parameterized_task 不是对象".to_string());
    }
    Ok(pt.to_string())
}

#[async_trait]
impl SubAgentResultCallback for SaveCallback {
    async fn on_result(&self, call: &SubAgentCall<'_>) -> ResultDecision {
        tracing::info!(
            "[{}] 子 agent '{}' 完成, tool_call_id={}, content_len={}, is_error={}",
            AGENT,
            call.ctx.agent_name,
            call.ctx.tool_call_id,
            call.text().len(),
            call.result.is_error,
        );

        // ── 取前置分析结论（解析 / 定稿判定 / 会话归属都已由它完成）──
        let analysis = match require_analysis(AGENT, call) {
            Ok(analysis) => analysis,
            Err(decision) => return decision,
        };

        // 读 step2 已登记的 parameterized_task，整段落库（不经 LLM 转抄）
        let payload = match self
            .service
            .load_state(&self.plan_id, &analysis.session_id)
            .await
        {
            Ok(Some((_, products))) => match extract_parameterized_task(&products) {
                Ok(p) => p,
                Err(reason) => {
                    return ResultDecision::Abort(format!("[{AGENT}] 落库取数失败：{reason}"))
                }
            },
            Ok(None) => {
                return ResultDecision::Abort(format!(
                    "[{AGENT}] 会话 {} 尚无流程状态记录",
                    analysis.session_id
                ))
            }
            Err(e) => {
                return ResultDecision::Abort(format!(
                    "[{AGENT}] 读取流程状态失败: {e}"
                ))
            }
        };

        if let Err(e) = self
            .service
            .save_snapshot(&self.plan_id, &analysis.session_id, &payload)
            .await
        {
            return ResultDecision::Abort(format!("[{AGENT}] 落库失败: {e}"));
        }

        // 推进 saved（无产物登记，空补丁）
        let patch: Map<String, Value> = Map::new();
        if let Err(reason) = commit_state(
            AGENT,
            &self.service,
            &self.plan_id,
            &analysis.session_id,
            Some(NEXT_STEP),
            &patch,
        )
        .await
        {
            return ResultDecision::Abort(reason);
        }

        hand_off(call)
    }

    fn name(&self) -> &str {
        AGENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本 step 独有的回归价值：从 products 整段取出 parameterized_task 且不丢嵌套字段。
    #[test]
    fn extracts_parameterized_task_verbatim() {
        let products = serde_json::json!({
            "task_definition": { "task": "建目录" },
            "parameterized_task": {
                "template": "在 ${filepath} 维护日志",
                "parameters": [{ "name": "filepath", "default": "C:/a/b/text.txt" }],
            },
        })
        .to_string();

        let payload = extract_parameterized_task(&products).unwrap();
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["template"], "在 ${filepath} 维护日志");
        assert_eq!(v["parameters"][0]["name"], "filepath");
    }

    #[test]
    fn missing_parameterized_task_is_error() {
        assert!(extract_parameterized_task("{}").is_err());
        assert!(extract_parameterized_task("not json").is_err());
        let no_pt = serde_json::json!({ "task_definition": {} }).to_string();
        assert!(extract_parameterized_task(&no_pt).is_err());
    }
}
