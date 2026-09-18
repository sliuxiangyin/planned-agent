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
        }
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

        let raw = match self.service.load_state(&self.plan_id, &session_id).await {
            Ok(Some((_, products))) => products,
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

        let inject = pick_injections(products.as_ref(), &self.mapping);
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
}
