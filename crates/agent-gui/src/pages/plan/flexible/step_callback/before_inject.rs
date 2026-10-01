//! 启动前注入回调：把 `flexible_state` 里已定稿的产物注入 step 子 agent 的调用参数。
//!
//! 目的：**消除「协调器 LLM 转抄」**。轨迹这类数据由系统从 state 直取后注入，不再经
//! 协调器「读一遍 → 再写进 tool_call 参数」的转抄路径 —— 该路径历史上把合法的 `null`
//! 抄成 `""`、把 `"array"` 抄成 `{"type":"array"}`。
//!
//! 注入语义（见 `planned_agent::chat::BeforeDecision::Inject`）：**顶层字段合并，同名 key
//! 由注入方覆盖**。因此即使协调器仍在传同名字段，系统数据也永远赢，不会被 LLM 的脏数据
//! 污染；同时「注入」只改写发给子 agent 的参数，不碰它的输出与 history。
//!
//! 降级策略：state 缺失 / 读库失败 / products 非法一律 **不注入 + warn**，让流程按原参数
//! 继续 —— 一次可容忍的数据缺失不该被升级成流程中断（故不用 `Abort`）。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{BeforeDecision, SubAgentBeforeCallback, SubAgentCallContext};
use serde_json::Value;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::{read_host_session_id, HOST_SESSION_ID_FIELD};

/// 从 `flexible_state.products` 取字段注入 task 参数的启动前回调。
///
/// `mapping` 为 `(state 产物字段名, 注入到 task 的字段名)`：两侧同名时写同一个标识符，
/// 需要改名时（如 `compressed_context` → `execution_trace_summary`）各写各的。
pub(crate) struct StateInjectCallback {
    plan_id: String,
    service: Arc<PlansFlexibleService>,
    mapping: Vec<(&'static str, &'static str)>,
    /// 需要把 `flexible_state.current_step` 一并注入时的目标字段名（默认不注入）。
    ///
    /// `current_step` 是 state 的**列**、不在 `products` 里，所以走不了 `mapping`：
    /// 只有明确需要它的 step（`flexible_revise` —— 据档位判断「改完要不要问用户
    /// 更新已保存的模板」）才用 [`Self::with_step_field`] 打开。
    step_field: Option<&'static str>,
}

impl StateInjectCallback {
    pub(crate) fn new(
        plan_id: String,
        service: Arc<PlansFlexibleService>,
        mapping: Vec<(&'static str, &'static str)>,
    ) -> Self {
        Self {
            plan_id,
            service,
            mapping,
            step_field: None,
        }
    }

    /// 追加注入 `flexible_state.current_step`（值如 `planned` / `saved`）。
    pub(crate) fn with_step_field(mut self, field: &'static str) -> Self {
        self.step_field = Some(field);
        self
    }
}

#[async_trait]
impl SubAgentBeforeCallback for StateInjectCallback {
    async fn before_start(&self, ctx: &SubAgentCallContext) -> BeforeDecision {
        let Some(session_id) = read_host_session_id(&ctx.arguments) else {
            tracing::warn!(
                "[flexible] {} 注入跳过：调用参数缺少 {}",
                ctx.agent_name,
                HOST_SESSION_ID_FIELD
            );
            return BeforeDecision::Continue;
        };

        let (current_step, raw) = match self.service.load_state(&self.plan_id, &session_id).await {
            Ok(Some((step, products))) => (step, products),
            Ok(None) => {
                tracing::warn!(
                    "[flexible] {} 注入跳过：flexible_state 无该会话记录",
                    ctx.agent_name
                );
                return BeforeDecision::Continue;
            }
            Err(e) => {
                tracing::warn!(
                    "[flexible] {} 注入跳过：flexible_state 读取失败: {}",
                    ctx.agent_name,
                    e
                );
                return BeforeDecision::Continue;
            }
        };

        let products = parse_products(&raw);
        for (src, _) in &self.mapping {
            if products.as_ref().is_none_or(|m| !m.contains_key(*src)) {
                tracing::warn!(
                    "[flexible] {} 注入缺字段：state 无 `{}`（该字段仍由原参数提供）",
                    ctx.agent_name,
                    src
                );
            }
        }

        let inject = build_injection(
            products.as_ref(),
            &self.mapping,
            &current_step,
            self.step_field,
        );
        if inject.is_empty() {
            return BeforeDecision::Continue;
        }
        tracing::info!(
            "[flexible] {} 注入 {} 个字段：{:?}",
            ctx.agent_name,
            inject.len(),
            inject.keys().collect::<Vec<_>>()
        );
        BeforeDecision::Inject(Value::Object(inject))
    }

    fn name(&self) -> &str {
        "flexible_state_inject"
    }
}

/// 解析 `flexible_state.products`（JSON 文本）为对象；非法 JSON / 非对象一律视为无数据。
fn parse_products(raw: &str) -> Option<serde_json::Map<String, Value>> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.as_object().cloned())
}

/// 按映射从 `products` 挑字段，组装注入对象。
///
/// 缺失的字段**跳过**（既不写 `null` 也不给默认值）——写 null 会把「上游没产出」伪装成
/// 「上游产出了空值」，让子 agent 误判。
fn pick_injections(
    products: Option<&serde_json::Map<String, Value>>,
    mapping: &[(&str, &str)],
) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    let Some(products) = products else {
        return out;
    };
    for (src, dst) in mapping {
        if let Some(v) = products.get(*src) {
            out.insert((*dst).to_string(), v.clone());
        }
    }
    out
}

/// 组装最终注入对象：先按 `mapping` 从 `products` 挑，再（如要求）补上 `current_step`。
///
/// `current_step` 拿的是 state 的列而不是 `products` 字段，所以不能并入 `mapping`；
/// 它**总是存在**（不像产物字段可能缺），故不看 `products` 是否为空。
fn build_injection(
    products: Option<&serde_json::Map<String, Value>>,
    mapping: &[(&str, &str)],
    current_step: &str,
    step_field: Option<&str>,
) -> serde_json::Map<String, Value> {
    let mut out = pick_injections(products, mapping);
    if let Some(field) = step_field {
        out.insert(
            field.to_string(),
            Value::String(current_step.to_string()),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn products(v: Value) -> serde_json::Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn picks_mapped_fields_only() {
        let p = products(json!({
            "execution_trace": [1, 2],
            "compressed_context": "摘要",
            "unrelated": 9
        }));
        let out = pick_injections(Some(&p), &[("execution_trace", "execution_trace")]);
        assert_eq!(Value::Object(out), json!({ "execution_trace": [1, 2] }));
    }

    #[test]
    fn mapping_can_rename() {
        let p = products(json!({ "compressed_context": "摘要" }));
        let out = pick_injections(
            Some(&p),
            &[("compressed_context", "execution_trace_summary")],
        );
        assert_eq!(
            Value::Object(out),
            json!({ "execution_trace_summary": "摘要" })
        );
    }

    #[test]
    fn missing_source_is_skipped_not_nulled() {
        let p = products(json!({ "compressed_context": "摘要" }));
        let out = pick_injections(Some(&p), &[("execution_trace", "execution_trace")]);
        assert!(out.is_empty());
    }

    #[test]
    fn missing_products_yields_empty_injection() {
        let out = pick_injections(None, &[("execution_trace", "execution_trace")]);
        assert!(out.is_empty());
    }

    #[test]
    fn non_object_products_parse_as_none() {
        assert!(parse_products("not json").is_none());
        assert!(parse_products("[1,2]").is_none());
        assert!(parse_products("{}").is_some());
    }

    /// `current_step` 与产物字段并排注入，且**即使 products 为空也会带上**
    /// （它来自 state 的列，不依赖产物）。
    #[test]
    fn step_field_rides_alongside_products() {
        let p = products(json!({ "steps": [{ "result_reference": "#E1" }] }));
        let out = build_injection(
            Some(&p),
            &[("steps", "current_steps")],
            "saved",
            Some("current_step"),
        );
        assert_eq!(
            Value::Object(out),
            json!({
                "current_steps": [{ "result_reference": "#E1" }],
                "current_step": "saved",
            })
        );

        let empty = build_injection(None, &[], "planned", Some("current_step"));
        assert_eq!(Value::Object(empty), json!({ "current_step": "planned" }));
    }

    /// 不开开关的 step 注入结果里**不含** `current_step`（避免它们无谓地看见档位）。
    #[test]
    fn step_field_absent_by_default() {
        let p = products(json!({ "steps": [] }));
        let out = build_injection(Some(&p), &[("steps", "steps")], "saved", None);
        assert_eq!(Value::Object(out), json!({ "steps": [] }));
    }
}
