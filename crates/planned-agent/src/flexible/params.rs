//! 运行参数表：一次执行的参数值，以及步骤 `intent` 的占位符展开。
//!
//! - 取值规则：用户填的 > 模板 `inputs[].default`；
//! - 展开：把 `steps[].intent` 里的 `${name}` 换成实际值（见 [`render_step_intent`]）。

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::Value;

use super::placeholder;
use super::template::{FlexiblePlanTemplate, PlanStep};

/// 一次执行的参数值（参数名 → 值）。
#[derive(Debug, Clone, Default)]
pub struct PlanRunParams {
    values: BTreeMap<String, Value>,
}

impl PlanRunParams {
    /// 新建空表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 用模板 `inputs[].default` 初始化。
    ///
    /// 无 `default` 的参数**不进表** —— 调用方需在 [`render_step_intent`] 处暴露缺值，
    /// 而不是在这里静默补一个空串。
    pub fn from_template(template: &FlexiblePlanTemplate) -> Self {
        let mut values = BTreeMap::new();
        for input in &template.inputs {
            if let Some(default) = &input.default {
                values.insert(input.name.clone(), default.clone());
            }
        }
        Self { values }
    }

    /// 设置一个参数值（覆盖默认值）。
    pub fn set(&mut self, name: impl Into<String>, value: Value) {
        self.values.insert(name.into(), value);
    }

    /// 取一个参数值。
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    /// 展开任意文本里的 `${name}`（用本表值的文本形式）。
    ///
    /// 严格版：缺值或占位符未定义都返回 `Err`。UI 边填边预览请用
    /// [`crate::flexible::render_lenient`]（容错、保留未填占位符）。
    pub fn render(&self, template: &str) -> Result<String> {
        placeholder::render(template, &self.as_text_map()).map_err(anyhow::Error::msg)
    }

    /// 转成 `${name}` 替换用的文本表：字符串取原文，其余取 JSON 字面量。
    fn as_text_map(&self) -> BTreeMap<String, String> {
        self.values
            .iter()
            .map(|(name, value)| (name.clone(), value_to_text(value)))
            .collect()
    }
}

/// 展开一个步骤的 `intent`：把 `${name}` 换成参数值。
///
/// - 缺值（占位符有定义但表里没有）→ `Err`（由 [`placeholder::render`] 报出）；
/// - 未定义占位符（`inputs` 里根本没有）→ `Err`。
///
/// 两种失败都带步骤的 `result_reference`，便于定位是哪个步骤、哪个占位符。
pub fn render_step_intent(step: &PlanStep, params: &PlanRunParams) -> Result<String> {
    params.render(&step.intent).map_err(|reason| {
        anyhow::anyhow!("展开步骤 {} 的 intent 失败：{reason}", step.result_reference)
    })
}

/// 把参数值转成注入文本：字符串取原文，其余取 JSON 字面量。
///
/// 数字 `1` → `"1"`（而非 `"\"1\""`），保证 `${default_index}` 展开后可读。
fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flexible::PlanInput;
    use serde_json::json;

    /// 含两个参数占位符的 intent。
    const INTENT_WITH_PARAMS: &str = "处理文件 ${file_path}，起始索引 ${default_index}";

    /// 样例模板：两个有默认值的参数 + 一个无默认值的参数。
    fn sample_template() -> FlexiblePlanTemplate {
        FlexiblePlanTemplate {
            task: "维护日志".to_string(),
            inputs: vec![
                PlanInput {
                    name: "file_path".to_string(),
                    default: Some(json!("C:/a/b.txt")),
                    description: None,
                },
                PlanInput {
                    name: "default_index".to_string(),
                    default: Some(json!(1)),
                    description: None,
                },
                PlanInput {
                    name: "no_default".to_string(),
                    default: None,
                    description: None,
                },
            ],
            steps: vec![PlanStep {
                result_reference: "#E1".to_string(),
                intent: INTENT_WITH_PARAMS.to_string(),
                expected_output: "${file_path} 内容".to_string(),
                dependencies: vec![],
            }],
        }
    }

    #[test]
    fn seeds_defaults_and_skips_missing() {
        let params = PlanRunParams::from_template(&sample_template());
        assert_eq!(params.get("file_path"), Some(&json!("C:/a/b.txt")));
        assert_eq!(params.get("default_index"), Some(&json!(1)));
        // 无 default 的参数不进表
        assert_eq!(params.get("no_default"), None);
    }

    #[test]
    fn set_overrides_default() {
        let mut params = PlanRunParams::from_template(&sample_template());
        params.set("file_path", json!("D:/x.txt"));
        assert_eq!(params.get("file_path"), Some(&json!("D:/x.txt")));
    }

    #[test]
    fn renders_string_and_number() {
        let tpl = sample_template();
        let params = PlanRunParams::from_template(&tpl);
        // 字符串取原文、数字取字面量（不带引号）
        assert_eq!(
            render_step_intent(&tpl.steps[0], &params).unwrap(),
            "处理文件 C:/a/b.txt，起始索引 1"
        );
    }

    #[test]
    fn reports_missing_value_with_step_reference() {
        let tpl = sample_template();
        // 空表：占位符有定义但没值
        let params = PlanRunParams::new();
        let err = render_step_intent(&tpl.steps[0], &params)
            .unwrap_err()
            .to_string();
        assert!(err.contains("#E1"), "错误应点名步骤: {err}");
        assert!(err.contains("${file_path}"), "错误应点名占位符: {err}");
    }

    #[test]
    fn rejects_undefined_placeholder() {
        let tpl = sample_template();
        let mut step = tpl.steps[0].clone();
        step.intent = "处理 ${ghost}".to_string();
        let params = PlanRunParams::from_template(&tpl);
        let err = render_step_intent(&step, &params).unwrap_err().to_string();
        assert!(err.contains("${ghost}"), "错误应点名未定义占位符: {err}");
    }
}
