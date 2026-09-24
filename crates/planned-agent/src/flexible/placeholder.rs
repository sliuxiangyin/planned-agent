//! `${name}` 占位符的收集、校验与运行时替换。
//!
//! 契约（与 `prompts/flexible/flexible_parameterize.toml` 一致）：
//! - 占位符语法固定为 `${name}`，`name` 必须与 `inputs[].name` 完全一致；
//! - 占位只出现在 `steps[].intent` / `steps[].expected_output` 里；
//! - **不得自创占位符**：`steps` 里出现而 `inputs` 未定义的 `${name}` 属于契约违背 ——
//!   本模块的校验一律**报错**，绝不静默留空（静默留空会把「定义缺失」伪装成「没有参数」）。
//!
//! 落库保存的是**带占位的模板**（模板 / 实例分离：一个计划可换多套参数值），
//! 实际展开由运行时替换 [`render`] 完成，调用方为 `params::render_step_intent`。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// 占位符里会被扫描的步骤字段。
const PLACEHOLDER_FIELDS: &[&str] = &["intent", "expected_output"];

/// 占位符里会被扫描的 `output_schema` 文本字段（与 agent-gui 侧 `placeholder.rs` 保持一致）。
const SCHEMA_PLACEHOLDER_FIELDS: &[&str] = &["goal", "success", "format"];

/// 按出现顺序收集一段文本里的 `${name}` 占位符名（同名去重）。
///
/// 未闭合的 `${`（没有对应的 `}`）不会被当成占位符 —— 校验与替换各自对它的处理见
/// [`validate`] / [`render`]。
pub fn collect_placeholders(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        // `$` 与 `{` 都是单字节 ASCII，故 index + 2 必然落在字符边界上。
        if bytes[index] == b'$' && bytes[index + 1] == b'{' {
            if let Some(offset) = text[index + 2..].find('}') {
                let name = &text[index + 2..index + 2 + offset];
                if !name.is_empty() && !names.iter().any(|existing| existing == name) {
                    names.push(name.to_string());
                }
                index += 2 + offset + 1;
                continue;
            }
        }
        index += 1;
    }
    names
}

/// 收集 `steps`（数组）里所有步骤的占位符名（去重保序）。
pub fn collect_from_steps(steps: &Value) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let Some(items) = steps.as_array() else {
        return names;
    };
    for item in items {
        for field in PLACEHOLDER_FIELDS {
            let Some(text) = item.get(*field).and_then(Value::as_str) else {
                continue;
            };
            for name in collect_placeholders(text) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

/// 收集 `output_schema`（对象）的文本字段里的占位符名（去重保序）。
///
/// 非对象（含 `null` 契约）返回空表。执行器用它把契约里的 `${name}` 渲染成实际值后
/// 再交给输出整理步。
pub fn collect_from_schema(schema: &Value) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let Some(object) = schema.as_object() else {
        return names;
    };
    for field in SCHEMA_PLACEHOLDER_FIELDS {
        let Some(text) = object.get(*field).and_then(Value::as_str) else {
            continue;
        };
        for name in collect_placeholders(text) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// 校验：`steps` 里出现的每个 `${name}` 都必须在 `inputs` 中有同名定义。
///
/// 返回 `Err` 时给出全部未定义占位符（便于协调器向用户复述原因）。
pub fn validate(steps: &Value, inputs: &Value) -> Result<(), String> {
    let defined: BTreeSet<&str> = inputs
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();

    let undefined: Vec<String> = collect_from_steps(steps)
        .into_iter()
        .filter(|name| !defined.contains(name.as_str()))
        .collect();

    if undefined.is_empty() {
        return Ok(());
    }
    Err(format!(
        "steps 中出现未定义的占位符: {}",
        undefined
            .iter()
            .map(|name| format!("${{{name}}}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// 运行时替换：把 `template` 里的 `${name}` 换成实际值。
///
/// - 未定义的占位符 → `Err`（不静默留空）；
/// - `${` 未闭合 → `Err`（按原文无法判断边界）。
pub fn render(template: &str, values: &BTreeMap<String, String>) -> Result<String, String> {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        rendered.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(format!("占位符未闭合: {}", &rest[start..]));
        };
        let name = &after[..end];
        match values.get(name) {
            Some(value) => rendered.push_str(value),
            None => return Err(format!("未定义的占位符: ${{{name}}}")),
        }
        rest = &after[end + 1..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

/// 宽容替换：缺失/为空的参数**保留 `${name}` 原样**，并回报缺失的占位符名。
///
/// 与 [`render`] 的分工：
/// - [`render`] 严格 —— 缺值即 `Err`，执行路径用它（缺参数就该失败，不能带病执行）；
/// - 本函数宽容 —— 供 UI「边填参数边预览」，允许半成品状态。
///
/// 空字符串视同缺失：用户清空输入框的意图是「还没填好」，保留占位符比替换成空更能说明问题。
/// `${` 未闭合时按原文保留（严格版在此处报错）。
pub fn render_lenient(template: &str, values: &BTreeMap<String, String>) -> (String, Vec<String>) {
    let mut missing: Vec<String> = Vec::new();
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        rendered.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            // 未闭合：按原文保留余下部分
            rendered.push_str(&rest[start..]);
            return (rendered, missing);
        };
        let name = &after[..end];
        match values.get(name) {
            Some(value) if !value.trim().is_empty() => rendered.push_str(value),
            _ => {
                // 缺值或空白：整段保留占位符原样，并记名（去重保序）
                rendered.push_str(&rest[start..start + 2 + end + 1]);
                if !missing.iter().any(|seen| seen.as_str() == name) {
                    missing.push(name.to_string());
                }
            }
        }
        rest = &after[end + 1..];
    }
    rendered.push_str(rest);
    (rendered, missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collects_names_in_order_without_duplicates() {
        let text = "在 ${filepath} 维护日志，写入 ${time_format}；再次打开 ${filepath}";
        assert_eq!(
            collect_placeholders(text),
            vec!["filepath".to_string(), "time_format".to_string()]
        );
    }

    #[test]
    fn ignores_unclosed_and_empty_placeholders() {
        assert!(collect_placeholders("没有占位符").is_empty());
        assert!(collect_placeholders("${未闭合").is_empty());
        assert!(collect_placeholders("${}").is_empty());
        // 单个 `$` 或裸 `{` 都不是占位符
        assert!(collect_placeholders("价格 $100 与 {A}").is_empty());
    }

    #[test]
    fn collects_from_steps_only_intent_and_expected_output() {
        let steps = json!([
            {
                "result_reference": "#E1",
                "intent": "读取 ${filepath}",
                "expected_output": "${filepath} 内容",
                "dependencies": []
            },
            {
                "result_reference": "#E2",
                "intent": "按 ${time_format} 格式化",
                "expected_output": "格式化结果",
                "dependencies": ["#E1"]
            }
        ]);
        assert_eq!(
            collect_from_steps(&steps),
            vec!["filepath".to_string(), "time_format".to_string()]
        );
    }

    #[test]
    fn validate_accepts_defined_placeholders() {
        let steps = json!([{ "intent": "读取 ${filepath}", "expected_output": "内容" }]);
        let inputs = json!([{ "name": "filepath", "default": "C:/a/b.txt" }]);
        assert!(validate(&steps, &inputs).is_ok());
    }

    #[test]
    fn validate_rejects_undefined_placeholder() {
        let steps = json!([{ "intent": "读取 ${filepath}", "expected_output": "写入 ${out_dir}" }]);
        let inputs = json!([{ "name": "filepath", "default": "C:/a/b.txt" }]);
        let err = validate(&steps, &inputs).unwrap_err();
        assert!(err.contains("${out_dir}"), "错误应点名未定义占位符: {err}");
        assert!(!err.contains("${filepath}"), "已定义的占位符不应出现在错误里: {err}");
    }

    #[test]
    fn validate_rejects_placeholder_without_inputs() {
        let steps = json!([{ "intent": "读取 ${filepath}", "expected_output": "内容" }]);
        assert!(validate(&steps, &json!([])).is_err());
    }

    #[test]
    fn render_substitutes_and_reports_missing() {
        let values = BTreeMap::from([("filepath".to_string(), "C:/a/b.txt".to_string())]);
        assert_eq!(
            render("读取 ${filepath} 内容", &values).unwrap(),
            "读取 C:/a/b.txt 内容"
        );
        let err = render("读取 ${other}", &values).unwrap_err();
        assert!(err.contains("${other}"), "{err}");
        assert!(render("读取 ${未闭合", &values).is_err());
    }

    #[test]
    fn lenient_keeps_unfilled_placeholders_and_reports_them() {
        let values = BTreeMap::from([
            ("filled".to_string(), "V".to_string()),
            ("blank".to_string(), "   ".to_string()),
        ]);
        let (text, missing) = render_lenient("A ${filled} B ${blank} C ${absent}", &values);
        // 已填的替换；空白与缺失的连占位符一起保留
        assert_eq!(text, "A V B ${blank} C ${absent}");
        assert_eq!(missing, vec!["blank".to_string(), "absent".to_string()]);
    }

    /// 同一占位符出现两次且未填时只报一次名。
    #[test]
    fn lenient_reports_each_missing_name_once() {
        let (_, missing) = render_lenient("${x} 与 ${x}", &BTreeMap::new());
        assert_eq!(missing, vec!["x".to_string()]);
    }

    #[test]
    fn lenient_keeps_unclosed_placeholder_verbatim() {
        let (text, missing) = render_lenient("读取 ${未闭合", &BTreeMap::new());
        assert_eq!(text, "读取 ${未闭合");
        assert!(missing.is_empty());
    }
}
