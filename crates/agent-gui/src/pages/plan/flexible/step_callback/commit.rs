//! 定稿落库的**公共纯工具**：产物补丁构建、写库 + 统一错误处理、链内交接。
//!
//! # 为什么只装纯工具，不装「统一的回调实现」
//!
//! 五个 step 的产物 / 清理 / 推进规则各自演化，回调编排留在各自的 `stepN/mod.rs`
//! （见 [`super`] 的说明）。这里只放**零策略**的那几段：同样的入参必然产生同样的行为，
//! 不掺任何 step 语义，也不持有状态 —— 各 step 拿自己的常量（`PRODUCTS` / `CLEAR` /
//! `NEXT_STEP`）来调用，差异依然写在各自文件里、一眼可见。
//!
//! 与已删除的 `step_commit.rs`（把五个 step 收敛成一份 `StepCallback` + `StepSpec`）的区别：
//! 那一版**掌管编排**（谁调、按什么顺序、填什么常量），这一版只是被各 step 调用的函数。

use planned_agent::chat::{ResultDecision, SubAgentCall};
use serde_json::{Map, Value};

use crate::services::plans_flexible_service::PlansFlexibleService;

/// 由定稿输出构造产物补丁。
///
/// - `products`：取输出 JSON 中的同名字段。**缺失或 `null` 一律跳过写入** ——
///   `merge_state` 把 `null` 当「删除」，直接写会在子 agent 漏字段时静默清掉已有产物。
/// - `clear`：把下游产物置 `null`（在 `products` 之后处理，故同名时以清除为准）。
///
/// `parsed` 应传前置分析规范化后的值（`StepAnalysis.parsed`），此处不再处理格式。
pub(crate) fn build_patch(
    agent: &str,
    parsed: &Value,
    products: &[&str],
    clear: &[&str],
) -> Map<String, Value> {
    let mut patch = Map::new();
    for key in products {
        match parsed.get(*key).filter(|v| !v.is_null()) {
            Some(value) => {
                patch.insert((*key).to_string(), value.clone());
            }
            None => tracing::warn!("[{}] 定稿但缺产物 '{}'，跳过该产物写入", agent, key),
        }
    }
    for key in clear {
        patch.insert((*key).to_string(), Value::Null);
    }
    patch
}

/// 写流程状态 + 统一错误处理。
///
/// - `Ok`：记一条 `info!` 日志（含推进后的 `current_step`），返回 `Ok(())`；
/// - `Err`：把存储错误包成**可直接交给 `Abort` 的理由**返回（含 agent / plan / 会话）。
///
/// 写库失败属**不可重试**的硬错误：重试同样会失败，而静默 `Accept` 会让协调器误以为该步
/// 已定稿（后续 step 会在缺产物的情况下继续）。故调用方拿到 `Err` 一律 `return Abort`。
pub(crate) async fn commit_state(
    agent: &str,
    service: &PlansFlexibleService,
    plan_id: &str,
    session_id: &str,
    next_step: Option<&str>,
    patch: &Map<String, Value>,
) -> Result<(), String> {
    match service
        .merge_state(plan_id, session_id, next_step, patch)
        .await
    {
        Ok((step, _)) => {
            tracing::info!(
                "[{}] 状态已登记: plan_id={}, host_session_id={}, current_step={}",
                agent,
                plan_id,
                session_id,
                step,
            );
            Ok(())
        }
        Err(e) => {
            let reason = format!(
                "[{}] 流程状态登记失败（plan_id={}, host_session_id={}）：{}",
                agent, plan_id, session_id, e
            );
            tracing::error!("{}", reason);
            Err(reason)
        }
    }
}

/// 链内交接：非末位把当前文本交给下一环，末位 [`ResultDecision::Accept`]。
///
/// 对外结果由前置分析定稿（[`super::prelude`]），回调不改它 —— 这里只决定「要不要继续传」。
pub(crate) fn hand_off(call: &SubAgentCall<'_>) -> ResultDecision {
    if call.is_last {
        ResultDecision::Accept
    } else {
        ResultDecision::Next(call.text().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn products_take_matching_fields_and_skip_null_or_missing() {
        // 本函数是五个 step 共用的公共行为，这里**一次**覆盖全部分支，
        // 各 step 只需再锁「自己的常量取值」。
        let parsed = serde_json::json!({
            "a": { "kept": 1 },
            "b": null,          // null → 跳过（不得当作删除）
            // "c" 缺失 → 跳过
        });
        let patch = build_patch("t", &parsed, &["a", "b", "c"], &[]);
        assert_eq!(patch["a"], serde_json::json!({ "kept": 1 }));
        assert_eq!(patch.get("b"), None, "null 值不得写入");
        assert_eq!(patch.get("c"), None, "缺失字段不得写入");
        assert_eq!(patch.len(), 1);
    }

    #[test]
    fn clear_marks_downstream_as_null_and_wins_over_product() {
        let parsed = serde_json::json!({ "a": 1, "b": 2 });
        let patch = build_patch("t", &parsed, &["a"], &["b", "c"]);
        assert_eq!(patch["a"], serde_json::json!(1));
        assert_eq!(patch.get("b"), Some(&Value::Null));
        assert_eq!(patch.get("c"), Some(&Value::Null));

        // 同一个 key 既在 products 又在 clear：以清除为准（顺序语义，别反了）。
        let both = build_patch("t", &parsed, &["a"], &["a"]);
        assert_eq!(both.get("a"), Some(&Value::Null));
    }

    #[test]
    fn empty_products_and_clear_yield_empty_patch() {
        let parsed = serde_json::json!({ "a": 1 });
        assert!(build_patch("t", &parsed, &[], &[]).is_empty());
    }

    #[test]
    fn values_are_cloned_verbatim() {
        // 补丁必须原样搬运（含嵌套里的 null —— step5 的模板副本靠这条）：
        // 规范化已在前置分析做过（`canonicalize_windows_paths`），此处不得再加工。
        let parsed = serde_json::json!({
            "k": { "deep": [1, { "expected_schema": null }] },
        });
        let patch = build_patch("t", &parsed, &["k"], &[]);
        assert_eq!(patch["k"], parsed["k"]);
        assert!(patch["k"]["deep"][1]["expected_schema"].is_null());
    }
}
