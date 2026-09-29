//! 运行参数表：一次执行的参数值，以及步骤 `intent` / `expected_output` 的占位符展开。
//!
//! - 取值规则：用户填的 > 模板 `inputs[].default`；
//! - 展开：把 `steps[].intent` 与 `steps[].expected_output` 里的 `${name}` 换成实际值
//!   （见 [`render_step_intent`] / [`render_step_expected_output`]）。
//!
//! 两个字段都必须展开：它们都会进 LLM 的 user 文本（见 `prompt::build_step_task`），
//! 未展开的 `${name}` 会让模型读到无法解析的占位符。

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

/// 展开一个步骤的 `expected_output`：把 `${name}` 换成参数值。
///
/// 语义与 [`render_step_intent`] **完全一致**（[`placeholder::render`] 的严格版：
/// 缺值或未定义占位符都 `Err`），失败信息同样带步骤的 `result_reference`。
///
/// **为什么不复用 `step.expected_output` 原文**：该字段有两种消费方 ——
/// 执行记录 / UI 展示要**模板原文**（`report::StepRunRecord::expected_output`），
/// 而发给 LLM 的 user 文本要**实际值**（`prompt::build_step_task`）。
/// 本函数产出后者；前者一律直接取 `step.expected_output`。
pub fn render_step_expected_output(step: &PlanStep, params: &PlanRunParams) -> Result<String> {
    params.render(&step.expected_output).map_err(|reason| {
        anyhow::anyhow!(
            "展开步骤 {} 的 expected_output 失败：{reason}",
            step.result_reference
        )
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
            output_schema: None,
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

    /// `expected_output` 里的 `${name}` 也要展开（`sample_template` 的 steps[0]
    /// 写的是 `"${file_path} 内容"`）。
    #[test]
    fn renders_step_expected_output() {
        let tpl = sample_template();
        let params = PlanRunParams::from_template(&tpl);
        let rendered = render_step_expected_output(&tpl.steps[0], &params).expect("应能展开");
        assert_eq!(rendered, "C:/a/b.txt 内容");
        assert!(!rendered.contains("${"), "不应残留占位符: {rendered}");
    }

    /// 缺值（定义了但表里没有）时 `expected_output` 展开失败，错误带字段名与占位符名。
    #[test]
    fn rejects_expected_output_with_missing_param() {
        let tpl = sample_template();
        let mut step = tpl.steps[0].clone();
        step.expected_output = "写入 ${no_default}".to_string();
        let params = PlanRunParams::from_template(&tpl);
        let err = render_step_expected_output(&step, &params)
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected_output"), "错误应点名字段: {err}");
        assert!(err.contains("no_default"), "错误应点名占位符: {err}");
    }
}
