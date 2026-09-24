//! `${name}` 占位符的收集、校验与运行时替换。
//!
//! 契约（与 `prompts/flexible/flexible_parameterize.toml` 一致）：
//! - 占位符语法固定为 `${name}`，`name` 必须与 `inputs[].name` 完全一致；
//! - 占位出现在 `steps[].intent` / `steps[].expected_output`，以及 `output_schema` 的
//!   `goal` / `success` / `format` 文本字段里；
//! - **不得自创占位符**：上述字段里出现而 `inputs` 未定义的 `${name}` 属于契约违背 ——
//!   本模块的校验一律**报错**，绝不静默留空（静默留空会把「定义缺失」伪装成「没有参数」）。
//!
//! 落库保存的是**带占位的模板**（模板 / 实例分离：一个计划可换多套参数值），
//! 实际展开由运行时替换 [`render`] 完成。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// 占位符里会被扫描的步骤字段。
const PLACEHOLDER_FIELDS: &[&str] = &["intent", "expected_output"];

/// 占位符里会被扫描的 `output_schema` 文本字段。
///
/// `required` / `wanted` 不在列：它们是**字段名**，不是含值的文本。
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

/// 收集 `output_schema`（对象）里所有文本字段的占位符名（去重保序）。
///
/// `None` / 非对象（含用户选「定不了」时的 `null`）一律返回空表。
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

/// 校验：`steps` 与 `output_schema` 里出现的每个 `${name}` 都必须在 `inputs` 中有同名定义。
///
/// 返回 `Err` 时给出全部未定义占位符并**点名来源**（`steps` / `output_schema`），
/// 便于协调器向用户复述原因。
///
/// `schema` 传 `None` 表示本次模板没有输出契约（用户跳过输出定义或选「定不了」）。
pub fn validate(steps: &Value, schema: Option<&Value>, inputs: &Value) -> Result<(), String> {
    let defined: BTreeSet<&str> = inputs
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();

    let mut undefined: Vec<(String, &str)> = Vec::new();
    let sources: [(&str, Vec<String>); 2] = [
        ("steps", collect_from_steps(steps)),
        (
            "output_schema",
            schema.map(collect_from_schema).unwrap_or_default(),
        ),
    ];
    for (source, names) in sources {
        for name in names {
            if !defined.contains(name.as_str()) {
                undefined.push((name, source));
            }
        }
    }

    if undefined.is_empty() {
        return Ok(());
    }
    Err(format!(
        "出现未定义的占位符: {}",
        undefined
            .iter()
            .map(|(name, source)| format!("${{{name}}}（{source}）"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// 运行时替换：把 `template` 里的 `${name}` 换成实际值。
///
/// - 未定义的占位符 → `Err`（不静默留空）；
/// - `${` 未闭合 → `Err`（按原文无法判断边界）。
///
/// 执行器尚未实现，故暂时没有生产调用点；保存的是带占位的模板，接线后由执行器调用。
#[allow(dead_code)]pub fn render(template: &str, values: &BTreeMap<String, String>) -> Result<String, String> {
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
        assert!(validate(&steps, None, &inputs).is_ok());
    }

    #[test]
    fn validate_rejects_undefined_placeholder() {
        let steps = json!([{ "intent": "读取 ${filepath}", "expected_output": "写入 ${out_dir}" }]);
        let inputs = json!([{ "name": "filepath", "default": "C:/a/b.txt" }]);
        let err = validate(&steps, None, &inputs).unwrap_err();
        assert!(err.contains("${out_dir}"), "错误应点名未定义占位符: {err}");
        assert!(err.contains("steps"), "错误应点名来源: {err}");
        assert!(!err.contains("${filepath}"), "已定义的占位符不应出现在错误里: {err}");
    }

    #[test]
    fn validate_rejects_placeholder_without_inputs() {
        let steps = json!([{ "intent": "读取 ${filepath}", "expected_output": "内容" }]);
        assert!(validate(&steps, None, &json!([])).is_err());
    }

    /// `output_schema` 也是占位符载体（`goal` / `success` / `format`），且错误里要点名来源。
    #[test]
    fn validate_covers_output_schema_text_fields() {
        let steps = json!([{ "intent": "向 ${file_path} 追加一行", "expected_output": "已追加" }]);
        let inputs = json!([{ "name": "file_path", "default": "C:/a/b.txt" }]);

        // 已定义 → 通过
        let ok = json!({ "kind": "bool", "goal": "向 ${file_path} 追加一行", "success": "已追加成功" });
        assert!(validate(&steps, Some(&ok), &inputs).is_ok());

        // 未定义 → 点名 output_schema
        let bad = json!({ "kind": "bool", "goal": "向 ${write_dir} 追加一行", "success": "已追加成功" });
        let err = validate(&steps, Some(&bad), &inputs).unwrap_err();
        assert!(err.contains("${write_dir}"), "{err}");
        assert!(err.contains("output_schema"), "错误应点名来源: {err}");

        // `null` 契约（用户选「定不了」）不得被当作占位符载体
        let null_schema = Value::Null;
        assert!(validate(&steps, Some(&null_schema), &inputs).is_ok());
    }

    /// `required` / `wanted` 是字段名而不是含值文本：里面写 `${x}` 不应当成占位符。
    #[test]
    fn schema_field_names_are_not_placeholder_carriers() {
        let schema = json!({
            "kind": "json",
            "goal": "抽字段",
            "required": ["${weird}"],
            "wanted": []
        });
        assert!(collect_from_schema(&schema).is_empty());
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
}
