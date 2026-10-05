//! 工具链记忆的**提炼侧**：把一次成功执行的报告压成「每步的形状化工具链」。
//!
//! 两个纯函数、一条链路：
//! - [`shape_arguments`]：反参数化 —— 把实参里的**本次参数值**换回 `${name}`。
//!   不这么做，下次执行会把「上次那个文件路径」当成标准答案照抄
//!   （见 `docs/planned-agent/flexible-tool-chain-memory.md` §4.1「本稿的命门」）。
//! - [`recipes_from_report`]：过滤 + 形状化，产出可直接落库的配方。
//!
//! 过滤与形状化的**全部策略**都收在 [`recipes_from_report`] 里：纯函数、可单测，
//! 不查库、不认识宿主（落库由 `run_service` 的写端口转交宿主完成）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::executor::RESOLVE_RESULT_REFERENCE;
use super::report::{PlanRunReport, StepRunRecord, StepStatus};
use crate::flexible::plan::params::PlanRunParams;

/// 参与反参数化的参数值**最短字符数**。
///
/// 更短的值不参与：数字 `1`、单字符会在任何 JSON 里命中一大片，替换等于毁掉入参。
const MIN_PARAM_TEXT_LEN: usize = 4;

/// 一次工具调用在配方里的形状。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallShape {
    /// 工具名，如 `read_file`。
    pub tool: String,
    /// 反参数化后的入参（JSON 文本）：本次参数值已换回 `${name}`。
    ///
    /// `None` = 入参无法形状化（超长被 `describe_arguments` 截断，或本就不是 JSON）——
    /// 这时**只留工具名**，绝不把截断的原文写进记忆。
    #[serde(default)]
    pub args_shape: Option<String>,
}

/// 单个步骤的配方 —— 即 `flexible_run_history` 一行里除主键与 `revision` 之外的内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepToolRecipe {
    /// 步骤标识（`#E1`），与 `parameterized_task.steps[].result_reference` 对齐。
    pub result_reference: String,
    /// 该步的工具链（按发生顺序，已形状化）。
    pub calls: Vec<ToolCallShape>,
}

/// 反参数化：把实参 JSON 里的**本次参数值**换回 `${name}`。
///
/// 返回 `None` = 入参不是合法 JSON（`describe_arguments` 对超长入参留了截断标记）——
/// 调用方据此只留工具名。
///
/// 必须走 JSON 解析、不能对文本直接做字符串替换：参数值里可能带 `"` 或 `\`，
/// 裸文本替换会把 JSON 结构改坏。只处理**值**（String 叶子），不动 key。
pub fn shape_arguments(args_json: &str, params: &PlanRunParams) -> Option<String> {
    shape_with_values(args_json, &params.as_text_map())
}

/// [`shape_arguments`] 的内部形态：值表由调用方备好，避免逐步重复构造。
fn shape_with_values(args_json: &str, values: &BTreeMap<String, String>) -> Option<String> {
    let value: Value = serde_json::from_str(args_json).ok()?;
    Some(rewrite(&value, values).to_string())
}

/// 从一次**成功**执行的报告提炼每步的工具链配方。
///
/// 返回空 vec 的情形（一律不落库）：整次执行未成功，或没有任何一步留下可用的工具链。
/// 五条过滤规则（`docs/planned-agent/flexible-tool-chain-memory.md` §5）：
/// 1. `success == false` → 整体不提炼 —— 表是覆盖语义，失败路径会把上次的好路径冲掉；
/// 2. 非 `Done` 的步跳过（防御；`success` 为真时本应全 `Done`）；
/// 3. `#RESULT` 输出整理步跳过 —— 系统生成的固定步，不注入，也不该占一行；
/// 4. 只收 `ok == true` 的调用 —— 失败调用是「试错」，配方要的是可复制的成功路径；
/// 5. 形状化后一条不剩的步跳过 —— 纯 LLM 步没有链可记，留空行只是垃圾。
pub fn recipes_from_report(
    report: &PlanRunReport,
    params: &PlanRunParams,
) -> Vec<StepToolRecipe> {
    if !report.success {
        return Vec::new();
    }
    let values = params.as_text_map();
    report
        .steps
        .iter()
        .filter(|step| step.status == StepStatus::Done)
        .filter(|step| step.result_reference != RESOLVE_RESULT_REFERENCE)
        .filter_map(|step| recipe_of_step(step, &values))
        .collect()
}

/// 单步 → 配方；没有可记的调用时返回 `None`（调用方据此跳过该步）。
fn recipe_of_step(
    step: &StepRunRecord,
    values: &BTreeMap<String, String>,
) -> Option<StepToolRecipe> {
    let calls: Vec<ToolCallShape> = step
        .tool_sequence
        .iter()
        .filter(|call| call.ok)
        .map(|call| ToolCallShape {
            tool: call.tool.clone(),
            args_shape: shape_with_values(&call.args, values),
        })
        .collect();

    (!calls.is_empty()).then(|| StepToolRecipe {
        result_reference: step.result_reference.clone(),
        calls,
    })
}

/// 深度遍历 JSON，把字符串叶子上的参数值换回 `${name}`。
///
/// 数字 / 布尔 / null 原样保留（第一版不还原非文本类型的参数）。
fn rewrite(value: &Value, params: &BTreeMap<String, String>) -> Value {
    match value {
        Value::String(text) => Value::String(rewrite_text(text, params)),
        Value::Array(items) => Value::Array(items.iter().map(|item| rewrite(item, params)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), rewrite(value, params)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// 把一段文本里出现的参数值换成 `${name}`。
///
/// **长的先换**：`${dir}` 的值若恰好是 `${file_path}` 值的前缀，先换短的就会把长值
/// 切成 `${dir}/report.txt` —— 那不是参数名，是垃圾。
fn rewrite_text(text: &str, params: &BTreeMap<String, String>) -> String {
    let mut pairs: Vec<(&str, &str)> = params
        .iter()
        .filter(|(_, value)| value.chars().count() >= MIN_PARAM_TEXT_LEN)
        .map(|(name, value)| (value.as_str(), name.as_str()))
        .collect();
    // 按值长度降序；等长时保持 `BTreeMap` 的 name 序（`sort_by` 稳定）→ 结果确定。
    pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let mut out = text.to_string();
    for (value, name) in pairs {
        out = out.replace(value, &format!("${{{name}}}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flexible::exec::report::{StepRunRecord, ToolCallRecord};
    use serde_json::json;

    /// 造一张参数表。
    fn params_of(pairs: &[(&str, Value)]) -> PlanRunParams {
        let mut params = PlanRunParams::new();
        for (name, value) in pairs {
            params.set(*name, value.clone());
        }
        params
    }

    fn call(tool: &str, args: &str, ok: bool) -> ToolCallRecord {
        ToolCallRecord {
            tool: tool.to_string(),
            args: args.to_string(),
            ok,
        }
    }

    fn step(reference: &str, status: StepStatus, calls: Vec<ToolCallRecord>) -> StepRunRecord {
        StepRunRecord {
            index: 1,
            result_reference: reference.to_string(),
            intent: "i".to_string(),
            expected_output: "o".to_string(),
            status,
            duration_ms: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            tool_calls: calls.len(),
            tool_sequence: calls,
            rounds: 0,
            llm_retries: 0,
            call_usages: vec![],
            output_summary: None,
            output: None,
            output_file: None,
            output_truncated: false,
            error: None,
        }
    }

    fn report(success: bool, steps: Vec<StepRunRecord>) -> PlanRunReport {
        PlanRunReport {
            success,
            total_duration_ms: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            tool_calls: 0,
            steps,
            result: None,
        }
    }

    // ───────────────────────── 反参数化 ─────────────────────────

    /// 字符串值被换回 `${name}`。不断言整体字符串 —— `serde_json::Map` 的 key 顺序
    /// 取决于是否开了 `preserve_order`，只断言片段。
    #[test]
    fn rewrites_parameter_value_inside_json() {
        let params = params_of(&[("file_path", json!("C:/work/report.txt"))]);
        let shaped = shape_arguments(r#"{"path":"C:/work/report.txt"}"#, &params).unwrap();
        assert!(shaped.contains(r#""path":"${file_path}""#), "{shaped}");
        assert!(!shaped.contains("C:/work"), "{shaped}");
    }

    /// 数组与嵌套对象里的叶子同样要换。
    #[test]
    fn rewrites_inside_arrays_and_nested_objects() {
        let params = params_of(&[("target", json!("build/output"))]);
        let shaped = shape_arguments(
            r#"{"cmd":["copy","build/output"],"o":{"p":"build/output"}}"#,
            &params,
        )
        .unwrap();
        assert!(!shaped.contains("build/output"), "{shaped}");
        assert_eq!(shaped.matches("${target}").count(), 2, "{shaped}");
    }

    /// 不是参数值的字面量必须原样留着（它才是「本次的真实取值」之外的稳定信息）。
    #[test]
    fn keeps_literals_that_are_not_parameters() {
        let params = params_of(&[("file_path", json!("C:/work/report.txt"))]);
        let shaped =
            shape_arguments(r#"{"path":"C:/work/report.txt","limit":3}"#, &params).unwrap();
        assert!(shaped.contains(r#""limit":3"#), "{shaped}");
        assert!(shaped.contains("${file_path}"), "{shaped}");
    }

    /// 短值不参与替换：数字 `1` 只有 1 个字符，替换会毁掉入参。
    #[test]
    fn short_parameter_values_are_left_alone() {
        let params = params_of(&[("n", json!(1))]);
        let shaped = shape_arguments(r#"{"n":1,"path":"a1b"}"#, &params).unwrap();
        assert_eq!(shaped, r#"{"n":1,"path":"a1b"}"#);
    }

    /// 长值先换：`${dir}` 是 `${file_path}` 的前缀时，不能把长值切碎。
    #[test]
    fn longer_parameter_value_wins_over_its_prefix() {
        let params = params_of(&[
            ("dir", json!("C:/work")),
            ("file_path", json!("C:/work/report.txt")),
        ]);
        let shaped = shape_arguments(r#"{"path":"C:/work/report.txt"}"#, &params).unwrap();
        assert!(shaped.contains("${file_path}"), "{shaped}");
        assert!(!shaped.contains("${dir}/report.txt"), "{shaped}");
    }

    /// 非 JSON 入参（`parse_arguments` 退回的 `Value::String`）仍是合法 JSON 串，可形状化。
    #[test]
    fn shapes_quoted_plain_string_arguments() {
        let params = params_of(&[("dir", json!("/var/log/app"))]);
        let shaped = shape_arguments(r#""list /var/log/app""#, &params).unwrap();
        assert_eq!(shaped, r#""list ${dir}""#);
    }

    /// 超长入参被 `describe_arguments` 截断后不是合法 JSON → `None`（只留工具名）。
    #[test]
    fn truncated_or_broken_arguments_yield_none() {
        let params = params_of(&[("file_path", json!("C:/work/report.txt"))]);
        assert!(shape_arguments(
            r#"{"content":"abc…（已截断，共 1200 字符）"#,
            &params
        )
        .is_none());
        assert!(shape_arguments("", &params).is_none());
    }

    // ───────────────────────── 提炼 ─────────────────────────

    /// 失败的执行整体不提炼 —— 覆盖语义下写进去会冲掉上次的好路径。
    #[test]
    fn failed_run_yields_no_recipes() {
        let report = report(
            false,
            vec![step(
                "#E1",
                StepStatus::Done,
                vec![call("read_file", r#"{"path":"x"}"#, true)],
            )],
        );
        assert!(recipes_from_report(&report, &params_of(&[])).is_empty());
    }

    /// 只收 `ok == true` 的调用：失败调用是试错噪声。
    #[test]
    fn keeps_only_successful_calls() {
        let report = report(
            true,
            vec![step(
                "#E1",
                StepStatus::Done,
                vec![
                    call("ls", r#"{"path":"/a"}"#, false),
                    call("read_file", r#"{"path":"/a/b.txt"}"#, true),
                ],
            )],
        );
        let recipes = recipes_from_report(&report, &params_of(&[]));
        assert_eq!(recipes.len(), 1);
        let tools: Vec<&str> = recipes[0].calls.iter().map(|c| c.tool.as_str()).collect();
        assert_eq!(tools, vec!["read_file"]);
    }

    /// `#RESULT` 整理步与空链步都不产出配方。
    #[test]
    fn skips_result_step_and_empty_chains() {
        let report = report(
            true,
            vec![
                // 纯 LLM 步：没调过工具
                step("#E1", StepStatus::Done, vec![]),
                step("#E2", StepStatus::Done, vec![call("read_file", "{}", true)]),
                // 输出整理步：系统生成的固定步
                step(
                    RESOLVE_RESULT_REFERENCE,
                    StepStatus::Done,
                    vec![call("builtin_read_file_lines", "{}", true)],
                ),
            ],
        );
        let recipes = recipes_from_report(&report, &params_of(&[]));
        assert_eq!(recipes.len(), 1);
        assert_eq!(recipes[0].result_reference, "#E2");
    }

    /// 端到端：链里的参数值在提炼时就被形状化。
    #[test]
    fn shapes_values_end_to_end() {
        let params = params_of(&[("file_path", json!("C:/work/report.txt"))]);
        let report = report(
            true,
            vec![step(
                "#E1",
                StepStatus::Done,
                vec![call("read_file", r#"{"path":"C:/work/report.txt"}"#, true)],
            )],
        );
        let recipes = recipes_from_report(&report, &params);
        assert_eq!(recipes.len(), 1);
        assert_eq!(
            recipes[0].calls[0].args_shape.as_deref(),
            Some(r#"{"path":"${file_path}"}"#)
        );
    }
}
