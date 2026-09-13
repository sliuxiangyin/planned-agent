//! 各 step 回调共用的「定稿登记」实现。
//!
//! 所有 `flexible_stepN` 子 agent 都按同一约定输出**纯 JSON 且顶层带 `status`**
//! （契约见各 `crates/agent-gui/prompts/flexible/flexible_stepN.toml`）。本模块把
//! 「读会话归属 → 判定是否定稿 → 写产物 → 推进 `current_step` → 清下游产物」
//! 收敛成一份代码，各 step 只提供自己的 [`StepSpec`]。
//!
//! 判定原则：**宁可「不写」也不写错** —— 非定稿 status、非合法 JSON、拿不到
//! `host_session_id` 一律跳过登记（只记日志），交由协调器 prompt 里的兜底 `save` 处理。
//!
//! 回调只负责**状态推进 / 定稿记录**（写 `flexible_state`），与「用户是否认同本次需求」
//! 无关：用户不认同就会重跑该 step，回调再登记一次即覆盖旧值，两者不冲突。
//! 模板落库（`plans_flexible`）不在这里做，仍由 `flexible_save_template` 工具负责。
//!
//! 设计背景见 `docs/chat-flexible-回调会话归属设计.md`。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{ResultDecision, SubAgentCallContext, SubAgentResultCallback};
use planned_agent_core::mcp::types::ToolResult;
use serde_json::{Map, Value};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::{read_host_session_id, HOST_SESSION_ID_FIELD};

/// 一个 step 的定稿契约。
pub(crate) struct StepSpec {
    /// 子 agent 工具名（仅用于日志），如 `"flexible_step3"`。
    pub agent: &'static str,
    /// 定稿 status：子 agent 输出顶层 `status` 等于该值才算定稿。
    pub ok_status: &'static str,
    /// 定稿后推进到的 `current_step` 档位。
    pub next_step: &'static str,
    /// 定稿时要登记的产物 key（值取输出 JSON 中的同名字段；缺失或 `null` 则跳过写入）。
    pub products: &'static [&'static str],
    /// 定稿时要清除（传 `null`）的下游产物 key。
    pub clear: &'static [&'static str],
}

/// 通用 step 回调：按 [`StepSpec`] 把子 agent 的定稿输出登记到 `flexible_state`。
pub(crate) struct StepCallback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（读/写流程中间状态）。
    service: Arc<PlansFlexibleService>,
    /// 本 step 的定稿契约。
    spec: StepSpec,
}

impl StepCallback {
    pub(crate) fn new(
        plan_id: String,
        service: Arc<PlansFlexibleService>,
        spec: StepSpec,
    ) -> Self {
        Self {
            plan_id,
            service,
            spec,
        }
    }
}

#[async_trait]
impl SubAgentResultCallback for StepCallback {
    async fn on_result(&self, ctx: &SubAgentCallContext, result: &ToolResult) -> ResultDecision {
        let spec = &self.spec;
        let text = result.content.as_str().unwrap_or("");
        tracing::info!(
            "[{}] 子 agent '{}' 完成, tool_call_id={}, content_len={}, is_error={}",
            spec.agent,
            ctx.agent_name,
            ctx.tool_call_id,
            text.len(),
            result.is_error,
        );

        // ── 会话归属：值来自父 agent 传入的原始参数（核心库只透传，语义由这里定义）──
        let Some(session_id) = read_host_session_id(&ctx.arguments) else {
            tracing::warn!(
                "[{}] 回调未拿到 {}（父 agent 是否按 schema 传参？），跳过状态登记",
                spec.agent,
                HOST_SESSION_ID_FIELD
            );
            return ResultDecision::Accept;
        };

        // ── 定稿判定：step 契约约定输出为纯 JSON ──
        let parsed: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "[{}] 输出非合法 JSON，跳过状态登记: {}",
                    spec.agent,
                    e
                );
                return ResultDecision::Accept;
            }
        };
        let status = parsed.get("status").and_then(serde_json::Value::as_str);
        if status != Some(spec.ok_status) {
            // 非定稿（error / back_to_* / empty_result / cancelled 等）：本次未产生有效定稿产物，
            // **不推进** current_step，也不动任何已有产物（协调器按 prompt 决定重试或取消）。
            tracing::warn!(
                "[{}] 非定稿 status（{:?} ≠ {:?}），不登记产物",
                spec.agent,
                status,
                spec.ok_status,
            );
            return ResultDecision::Accept;
        }

        // ── 登记产物：缺失即跳过写入（保留原值）──
        // 否则 `merge_state` 会把 `null` 当「删除」，在子 agent 漏字段时静默清掉已有产物。
        let mut patch = Map::new();
        for key in spec.products {
            match parsed.get(*key).filter(|v| !v.is_null()) {
                Some(value) => {
                    patch.insert((*key).to_string(), value.clone());
                }
                None => tracing::warn!(
                    "[{}] 定稿但缺产物 '{}'，跳过该产物写入",
                    spec.agent,
                    key
                ),
            }
        }
        // ── 清下游：重做本 step ⇒ 其下游各阶段的定稿产物作废 ──
        for key in spec.clear {
            patch.insert((*key).to_string(), Value::Null);
        }

        match self
            .service
            .merge_state(&self.plan_id, &session_id, Some(spec.next_step), &patch)
            .await
        {
            Ok((step, _)) => tracing::info!(
                "[{}] 状态已登记: plan_id={}, host_session_id={}, current_step={}",
                spec.agent,
                self.plan_id,
                session_id,
                step,
            ),
            Err(e) => tracing::error!(
                "[{}] 登记状态失败: plan_id={}, host_session_id={}, err={}",
                spec.agent,
                self.plan_id,
                session_id,
                e,
            ),
        }

        ResultDecision::Accept
    }
}
