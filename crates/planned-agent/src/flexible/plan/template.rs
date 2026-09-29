//! 灵活计划模板的强类型定义。
//!
//! 与 `flexible_save` 落库的 JSON（`plans_flexible_sessions.parameterized_task`）
//! 一一对应：`{ task, inputs, steps }`。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 灵活计划模板（落库形态）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlexiblePlanTemplate {
    /// 任务描述（step1 定稿产物）
    pub task: String,
    /// 参数表（缺失视为空表）
    #[serde(default)]
    pub inputs: Vec<PlanInput>,
    /// 步骤骨架（可变值已写成 `${name}`）
    pub steps: Vec<PlanStep>,
    /// 输出契约（`flexible_output` 定稿产物）。
    ///
    /// `None` 有二义（都表示「没有结果契约」，消费方同等对待）：
    /// - 模板没走过输出定义步（用户跳过）；
    /// - 走过但用户选了「现在还定不了」。
    ///
    /// **必须带 `#[serde(default)]`**：早于输出步落库的模板 JSON 没有这个字段。
    #[serde(default)]
    pub output_schema: Option<Value>,
}

/// 一个参数的定义（对应 `inputs[]` 的一项）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanInput {
    /// 参数名（`${name}` 中的 `name`）
    pub name: String,
    /// 默认值（缺省表示必须由调用方提供）
    #[serde(default)]
    pub default: Option<Value>,
    /// 参数说明
    #[serde(default)]
    pub description: Option<String>,
}

/// 一个步骤（对应 `steps[]` 的一项）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    /// 结果引用标识，如 `#E1`（计划内唯一）
    pub result_reference: String,
    /// 子目标描述，可能含 `${name}` 占位符
    pub intent: String,
    /// 可验证产出（"做到什么样算完成"）
    pub expected_output: String,
    /// 依赖的前序 `result_reference`
    #[serde(default)]
    pub dependencies: Vec<String>,
}

impl FlexiblePlanTemplate {
    /// 从落库的 JSON 文本反序列化。
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("灵活计划模板 JSON 解析失败")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实落库形态：含 string 与 number 两种 default。
    ///
    /// 内容含 `"#E1"`，raw string 必须用 `r##"..."##` ——
    /// `r#"..."#` 会被 `"#` 提前终止。
    const SAMPLE: &str = r##"{
      "task": "在 Downloads 目录下维护 text.txt 文件",
      "inputs": [
        {
          "default": "C:/Users/wodpp/Desktop/Downloads/text.txt",
          "description": "要维护的目标文件的完整路径",
          "name": "file_path"
        },
        {
          "default": 1,
          "description": "默认起始索引",
          "name": "default_index"
        }
      ],
      "steps": [
        {
          "dependencies": [],
          "expected_output": "得到当前索引值",
          "intent": "处理文件 ${file_path}",
          "result_reference": "#E1"
        },
        {
          "dependencies": ["#E1"],
          "expected_output": "${file_path} 末尾新增一行",
          "intent": "基于 #E1 的索引值写入",
          "result_reference": "#E2"
        }
      ]
    }"##;

    #[test]
    fn parses_real_saved_template() {
        let tpl = FlexiblePlanTemplate::from_json(SAMPLE).expect("样例应能解析");

        assert_eq!(tpl.inputs.len(), 2);
        assert_eq!(tpl.inputs[0].name, "file_path");
        assert_eq!(
            tpl.inputs[0].default,
            Some(serde_json::json!("C:/Users/wodpp/Desktop/Downloads/text.txt"))
        );
        assert_eq!(tpl.inputs[1].default, Some(serde_json::json!(1)));

        assert_eq!(tpl.steps.len(), 2);
        assert_eq!(tpl.steps[0].result_reference, "#E1");
        assert_eq!(tpl.steps[1].dependencies, vec!["#E1".to_string()]);
    }

    /// `dependencies` 缺失时必须按空数组处理（`#[serde(default)]`）。
    #[test]
    fn missing_dependencies_defaults_to_empty() {
        let json = r##"{
          "task": "t",
          "steps": [
            { "result_reference": "#E1", "intent": "i", "expected_output": "o" }
          ]
        }"##;
        let tpl = FlexiblePlanTemplate::from_json(json).expect("缺 dependencies 不应报错");
        assert!(tpl.steps[0].dependencies.is_empty());
    }

    /// `inputs` 缺失时按空表处理。
    #[test]
    fn missing_inputs_defaults_to_empty() {
        let json = r#"{
          "task": "t",
          "steps": [{ "result_reference": "E1", "intent": "i", "expected_output": "o" }]
        }"#;
        let tpl = FlexiblePlanTemplate::from_json(json).expect("缺 inputs 不应报错");
        assert!(tpl.inputs.is_empty());
    }

    #[test]
    fn invalid_json_is_error() {
        assert!(FlexiblePlanTemplate::from_json("not json").is_err());
        // 缺必填 steps
        assert!(FlexiblePlanTemplate::from_json(r#"{"task":"t"}"#).is_err());
    }

    /// 输出契约字段：缺失 → `None`（旧落库数据）、显式 `null` → `None`（「跳过」与「定不了」同义）、
    /// 有值时原样保留并能序列化往返。
    #[test]
    fn output_schema_is_optional_and_round_trips() {
        // 旧模板（落库 JSON 里根本没有这个字段）必须仍能解析
        let legacy = FlexiblePlanTemplate::from_json(SAMPLE).expect("旧模板应能解析");
        assert!(
            legacy.output_schema.is_none(),
            "旧落库数据缺该字段 → None，不得报错"
        );

        // 显式 null（用户选「现在还定不了」，或不走输出步）
        let nulled = FlexiblePlanTemplate::from_json(
            r##"{
              "task": "t",
              "steps": [{ "result_reference": "#E1", "intent": "i", "expected_output": "o" }],
              "output_schema": null
            }"##,
        )
        .expect("null 应能解析");
        assert!(nulled.output_schema.is_none());

        // 有值：原样保留 + 往返
        let tpl = FlexiblePlanTemplate::from_json(
            r##"{
              "task": "t",
              "steps": [{ "result_reference": "#E1", "intent": "i", "expected_output": "o" }],
              "output_schema": {
                "kind": "csv",
                "description": "商品清单",
                "detail": "UTF-8，首行表头",
                "required": ["title"],
                "wanted": ["stock"]
              }
            }"##,
        )
        .expect("应能解析");
        let schema = tpl.output_schema.as_ref().expect("应有 output_schema");
        assert_eq!(schema["kind"], "csv");
        assert_eq!(schema["required"][0], "title");
        assert_eq!(schema["wanted"][0], "stock");

        let re_encoded = serde_json::to_string(&tpl).expect("应能序列化");
        let back = FlexiblePlanTemplate::from_json(&re_encoded).expect("往返后应能解析");
        assert_eq!(back, tpl);
    }
}
