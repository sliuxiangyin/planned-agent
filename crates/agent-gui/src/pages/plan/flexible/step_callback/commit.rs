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
use planned_agent::flexible::OutputSchema;
use serde_json::{json, Map, Value};

use crate::pages::plan::shared::session::TemplateNotifier;
use crate::services::plans_flexible_service::PlansFlexibleService;

use super::super::placeholder;

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
/// - `Ok`：记一条 `info!` 日志（含推进后的 `current_step`），返回合并后的 `(current_step, products)`；
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
) -> Result<(String, String), String> {
    match service
        .merge_state(plan_id, session_id, next_step, patch)
        .await
    {
        Ok((step, products)) => {
            tracing::info!(
                "[{}] 状态已登记: plan_id={}, host_session_id={}, current_step={}",
                agent,
                plan_id,
                session_id,
                step,
            );
            // 合并后的 `products` 一并返回：需要紧接着落库的调用方（如 `flexible_revise`
            // 在已保存状态下的模板同步）要它，否则得再读一次库。
            Ok((step, products))
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

/// 从 `flexible_state.products`（JSON 文本）组装落库 payload：
/// `{ task, inputs, steps, output_schema, category }`。
///
/// - `task` ← `task_definition.task`（需求澄清定稿产物）
/// - `inputs` ← `inputs`（参数化产出的参数表；缺失视为空表）
/// - `steps` ← `steps`（参数化后的步骤骨架，可变值已写成 `${name}`）
/// - `output_schema` ← `output_schema`（输出定义定稿的输出契约）。**缺失或 `null` 一律落 `null`** ——
///   用户跳过输出定义、或选了「现在还定不了」，都是合法情况，不得报错；非 `null` 时必须是对象。
/// - `category` ← `category`（计划步定稿的计划分类 / 技能）。**缺失或 `null` 一律落 `null`**（= 不分类）；
///   非 `null` 时必须是字符串；未知取值由执行期 `PlanCategory` 的 `#[serde(other)]` 兜成 `Other`。
///
/// 落库前校验 `steps` 里的 `${name}` 都能在 `inputs` 中找到同名定义：未定义即报错，
/// 不静默留空 —— 否则「参数漏定义」会被伪装成「本来就没有参数」。
///
/// 放在本层的理由：**多个 step 共用** —— `flexible_save` 用它首次落库，
/// `flexible_revise` 用它把修订同步回已保存的模板（见 [`persist_template`]）。
pub(crate) fn build_payload(products: &str) -> Result<String, String> {
    let parsed: Value =
        serde_json::from_str(products).map_err(|e| format!("products 非法 JSON: {e}"))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| "products 不是对象".to_string())?;

    let task = obj
        .get("task_definition")
        .and_then(|definition| definition.get("task"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|task| !task.is_empty())
        .ok_or_else(|| "state 缺 task_definition.task（step1 尚未定稿）".to_string())?;

    let steps = match obj.get("steps") {
        Some(steps) if !steps.is_null() => steps,
        _ => return Err("state 缺 steps（计划步尚未定稿）".to_string()),
    };
    if steps.as_array().is_none_or(|items| items.is_empty()) {
        return Err("steps 必须是非空数组".to_string());
    }

    let inputs = match obj.get("inputs") {
        Some(inputs) if !inputs.is_null() => inputs.clone(),
        _ => Value::Array(Vec::new()),
    };
    if inputs.as_array().is_none() {
        return Err("inputs 必须是数组".to_string());
    }

    // 输出契约可选：跳过输出定义 / 用户选「定不了」都落 null（两者语义等同，见
    // `docs/planned-agent/flexible-output-step.md` §5）。
    // 非 null 时走契约的**唯一定义处**校验（`kind` 合法 + 按 kind 的必填项）。
    let (output_schema, schema_for_placeholders) = match obj.get("output_schema") {
        Some(schema) if !schema.is_null() => {
            OutputSchema::parse(schema)?;
            (schema.clone(), Some(schema))
        }
        _ => (Value::Null, None),
    };

    // 计划分类可选：缺失 / `null` 落 `null`（= 不分类，不加规范段）；非空时必须是字符串。
    // 未知取值（如将来新增的分类名）不在此拦，交由执行期 `PlanCategory` 的 `#[serde(other)]` 兜成 `Other`。
    let category = match obj.get("category") {
        Some(category) if !category.is_null() => {
            if !category.is_string() {
                return Err("category 必须是字符串或 null".to_string());
            }
            category.clone()
        }
        _ => Value::Null,
    };

    // 占位符校验同时覆盖 `steps` 与 `output_schema` 的文本字段
    placeholder::validate(steps, schema_for_placeholders, &inputs)?;

    Ok(json!({
        "task": task,
        "inputs": inputs,
        "steps": steps,
        "output_schema": output_schema,
        "category": category,
    })
    .to_string())
}

/// 把 `flexible_state` 的完整产物组装成模板 payload 并落库，成功后通知左侧面板重读模板。
///
/// 落库前由 [`build_payload`] 把关（输出契约形态 + 占位符一致性）；**组装 / 校验失败时不会写库**。
///
/// 调用方负责「什么时候该落库」：
/// - `flexible_save`：首次创建流程定稿时；
/// - `flexible_revise`：`current_step` 已是 `saved` 时 —— 修订改了 `flexible_state.products`
///   就必须同步回 `plans_flexible_sessions`，否则模板停在旧值、左侧面板也看不到新参数。
pub(crate) async fn persist_template(
    service: &PlansFlexibleService,
    notifier: &TemplateNotifier,
    plan_id: &str,
    session_id: &str,
    products: &str,
) -> Result<(), String> {
    let payload = build_payload(products)?;

    service
        .save_snapshot(plan_id, session_id, &payload)
        .await
        .map_err(|e| format!("落库失败: {e}"))?;

    // 落库成功 → 立即通知左侧面板重读模板（左侧面板按模板版本号自动重读 `parameterized_task`）。
    notifier.notify();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ──────────────────── `build_payload`：state 产物 → 落库 payload ────────────────────

    /// `task` 取 `task_definition.task`；`inputs` / `steps` 原样落；缺 `output_schema` 落 `null`
    /// （跳过输出定义、或用户选「现在还定不了」，两者语义等同）。
    #[test]
    fn payload_takes_task_from_definition_and_nulls_missing_schema() {
        let products = json!({
            "task_definition": { "task": "读取日志并汇总错误" },
            "inputs": [{
                "name": "file_path",
                "default": "/var/log/app.log",
                "description": "日志文件路径",
            }],
            "steps": [{
                "result_reference": "#E1",
                "intent": "读取 ${file_path}",
                "expected_output": "日志内容",
                "dependencies": [],
            }],
        })
        .to_string();

        let payload = build_payload(&products).expect("齐全的三件套应可组装");
        let v: Value = serde_json::from_str(&payload).unwrap();

        assert_eq!(v["task"], "读取日志并汇总错误");
        assert_eq!(v["inputs"][0]["name"], "file_path");
        assert_eq!(v["steps"][0]["intent"], "读取 ${file_path}");
        assert!(v["output_schema"].is_null(), "无输出定义 → null");
    }

    /// `category` 可选：缺失 / `null` ⇒ `null`（不分类）；给了就原样落；非字符串报错。
    #[test]
    fn payload_carries_optional_category() {
        let with_category = |category: Value| {
            json!({
                "task_definition": { "task": "t" },
                "steps": [{
                    "result_reference": "#E1",
                    "intent": "做事",
                    "expected_output": "结果",
                    "dependencies": [],
                }],
                "category": category,
            })
            .to_string()
        };

        let v: Value =
            serde_json::from_str(&build_payload(&with_category(json!("File"))).unwrap()).unwrap();
        assert_eq!(v["category"], "File");

        let v: Value =
            serde_json::from_str(&build_payload(&with_category(Value::Null)).unwrap()).unwrap();
        assert!(v["category"].is_null(), "null → 不分类");

        assert!(
            build_payload(&with_category(json!({ "bad": 1 }))).is_err(),
            "非字符串应被拒"
        );

        // 完全缺该字段 ⇒ 同样落 `null`
        let no_key = json!({
            "task_definition": { "task": "t" },
            "steps": [{
                "result_reference": "#E1",
                "intent": "做事",
                "expected_output": "结果",
                "dependencies": [],
            }],
        })
        .to_string();
        let v: Value = serde_json::from_str(&build_payload(&no_key).unwrap()).unwrap();
        assert!(v["category"].is_null(), "缺字段 → 不分类");
    }

    /// `inputs` 缺失 ⇒ 空表（「本来就没有参数」，不是错误）。
    #[test]
    fn payload_falls_back_to_empty_inputs() {
        let products = json!({
            "task_definition": { "task": "t" },
            "steps": [{
                "result_reference": "#E1",
                "intent": "做事",
                "expected_output": "结果",
                "dependencies": [],
            }],
        })
        .to_string();

        let payload = build_payload(&products).unwrap();
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["inputs"], json!([]));
    }

    /// 缺 `task` ⇒ 报错（不落一个没有任务描述的模板）。
    #[test]
    fn payload_rejects_missing_task() {
        let products = json!({
            "steps": [{
                "result_reference": "#E1",
                "intent": "做事",
                "expected_output": "结果",
                "dependencies": [],
            }],
        })
        .to_string();

        let err = build_payload(&products).unwrap_err();
        assert!(err.contains("task_definition"), "实际：{err}");
    }

    /// 空 `steps` ⇒ 报错（没有步骤的计划没有意义）。
    #[test]
    fn payload_rejects_empty_steps() {
        let products = json!({
            "task_definition": { "task": "t" },
            "steps": [],
        })
        .to_string();

        let err = build_payload(&products).unwrap_err();
        assert!(err.contains("steps"), "实际：{err}");
    }

    /// `steps` 里的 `${name}` 没有同名 `inputs` 定义 ⇒ 报错。
    /// **这正是挡「模型自创占位符」的那道闸** —— 修订时改了 `intent` 文本也可能触发它。
    #[test]
    fn payload_rejects_undefined_placeholder() {
        let products = json!({
            "task_definition": { "task": "t" },
            "inputs": [],
            "steps": [{
                "result_reference": "#E1",
                "intent": "读取 ${nope}",
                "expected_output": "结果",
                "dependencies": [],
            }],
        })
        .to_string();

        assert!(
            build_payload(&products).is_err(),
            "未定义的占位符必须被拒，否则「参数漏定义」会被伪装成「本来就没有参数」"
        );
    }

    /// 非法 JSON ⇒ 报错。
    #[test]
    fn payload_rejects_broken_json() {
        assert!(build_payload("{ not json").unwrap_err().contains("JSON"));
    }

    // ───────────────────────────── 产物补丁 ─────────────────────────────

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
