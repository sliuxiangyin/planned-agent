//! flexible_step5 结果回调：模板编译成功后把状态推进到 `templated`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`。
//!
//! 定稿判定（与 `flexible_step5.toml` 的输出契约一致）：
//! - `{"status":"success", ...}` → 推进 `current_step = "templated"`，并把**整段输出**
//!   登记为产物 `template_payload`（模板副本）。
//! - 其它（`status:"error"` / 非 JSON）→ **不登记**。
//!
//! 职责边界：本回调**不做**模板落库。`plans_flexible_sessions` 的写入（含
//! `steps` / `execution_plan` 结构校验）仍由 `flexible_save_template` 工具负责——只有工具侧
//! 才知道落库是否成功。回调只承担「状态推进 / 定稿记录」加存一份模板副本，
//! 副本的意义是让落库工具**直接从状态取模板**，不必经过协调器 LLM 转抄
//! （转抄会改坏字段，例如把 `expected_schema: null` 写成 `""`）。
//!
//! 本 step 与其它 step 的差异：登记的是**整段定稿输出**（不是输出里的某个字段），
//! 也不清任何下游产物 —— 故不用 [`super::commit::build_patch`]，就地构造补丁；
//! 写库与交接仍走公共工具，解析与守门在 [`super::prelude`]。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{
    ResultDecision, SubAgentCall, SubAgentResultCallback, SubAgentResultChain,
};
use serde_json::Map;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::analysis::require_analysis;
use super::commit::{commit_state, hand_off};
use super::prelude::FlexibleStepPrelude;

/// 子 agent 工具名（日志用）。
const AGENT: &str = "flexible_step5";
/// 定稿 status：输出顶层 `status` 等于它才算定稿。
const OK_STATUS: &str = "success";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "templated";
/// 模板副本在 `flexible_state.products` 里的 key。
const PAYLOAD_KEY: &str = "template_payload";

/// `flexible_step5` 的定稿登记回调。
struct Step5Callback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态）。
    service: Arc<PlansFlexibleService>,
}

/// 由定稿输出构造本 step 的产物补丁：**整段输出原样**存一份（模板副本）。
///
/// 不做「取同名字段」的挑选，`null` 必须原样保留（`expected_schema: null` 是合法取值，
/// 改成 `""` 就是坏数据）；也不清下游产物（本档位已是最末的执行前档位）。
///
/// `parsed` 已经过前置分析的规范化（去围栏 / 路径正斜杠），此处不再处理格式。
fn build_payload_patch(parsed: &serde_json::Value) -> Map<String, serde_json::Value> {
    // `parsed` 必然是 JSON 对象（prelude 的解析只接受对象），直接原样存，不猜结构、不改值。
    let mut patch = Map::new();
    patch.insert(PAYLOAD_KEY.to_string(), parsed.clone());
    patch
}

#[async_trait]
impl SubAgentResultCallback for Step5Callback {
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

        // 整段定稿输出存为模板副本（原样，含 null）
        let patch = build_payload_patch(analysis.parsed);
        if let Err(reason) = commit_state(
            AGENT,
            &self.service,
            &self.plan_id,
            analysis.session_id,
            Some(NEXT_STEP),
            &patch,
        )
        .await
        {
            // 写库失败不可重试，如实上报：静默 Accept 会让协调器以为副本已就位，
            // 随后 `flexible_save_template` 会取不到东西。
            return ResultDecision::Abort(reason);
        }

        // 对外结果不由这里决定（前置分析已定稿）：只决定要不要把值交给下一位。
        hand_off(call)
    }

    fn name(&self) -> &str {
        AGENT
    }
}

/// 创建 `flexible_step5` 结果链（方便传给 `register_sub_agent`）。
///
/// 链上两环：**前置分析**（解析 + 守门 + 定稿对外文本）+ 本文件内的定稿登记回调。
pub fn create_step5_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> SubAgentResultChain {
    SubAgentResultChain::new(vec![Arc::new(Step5Callback { plan_id, service })])
        .with_prelude(Arc::new(FlexibleStepPrelude::new(AGENT, OK_STATUS)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本 step 独有的回归价值：**整段输出原样入 `template_payload`，`null` 不被改写**。
    #[test]
    fn stores_whole_output_verbatim_and_keeps_null() {
        // 模板副本必须整段原样：`null` 曾因协调器 LLM 转抄被写成 ""（改由此处直接落库）。
        let parsed = serde_json::json!({
            "status": "success",
            "steps": [],
            "execution_plan": [{ "expected_schema": null }],
        });
        let patch = build_payload_patch(&parsed);
        assert_eq!(patch.len(), 1);
        assert_eq!(patch.get(PAYLOAD_KEY), Some(&parsed));
        assert!(
            patch[PAYLOAD_KEY]["execution_plan"][0]["expected_schema"].is_null(),
            "模板副本必须原样保留 null（不得被改写成 \"\"）"
        );
        assert_eq!(NEXT_STEP, "templated");
        assert_eq!(OK_STATUS, "success");
    }
}
