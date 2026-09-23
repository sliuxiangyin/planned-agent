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
}
