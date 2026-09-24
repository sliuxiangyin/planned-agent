//! `flexible_save` 的定稿登记回调：保存定稿后把三件套落库并推进 `saved`。
//!
//! 归属定位：从 `SubAgentCallContext.arguments` 读取 `host_session_id`。
//!
//! 定稿判定（与 `flexible_save.toml` 的输出契约一致）：`{"status":"saved", ...}` 才算定稿；
//! 其它（`status:"error"` / 非 JSON）**不登记、不落库**。
//!
//! 职责边界：本回调**直接落库** —— 从 `flexible_state.products` 组装
//! `{ task, inputs, steps, output_schema }` 写入 `plans_flexible_sessions.parameterized_task` 列，
//! 不经协调器 LLM 转抄（转抄会改坏字段）。落库前两道校验：
//! 1. `output_schema` 形态（[`OutputSchema::parse`]：kind 合法 + 按 kind 的必填项）；
//! 2. `${name}` 占位符 —— `steps` 与 `output_schema` 的文本字段（`goal` / `success` / `format`）
//!    都必须有同名 `inputs` 定义（[`super::super::super::placeholder`]），挡掉「模型自创占位符」。
//!
//! 保存的是**带占位的模板**（模板 / 实例分离）：一个计划可换多套参数值，
//! 运行时由 `${name}` 替换展开。

use std::sync::Arc;

use async_trait::async_trait;
use planned_agent::chat::{ResultDecision, SubAgentCall, SubAgentResultCallback};
use planned_agent::flexible::OutputSchema;
use serde_json::{json, Map, Value};

use crate::pages::plan::shared::session::TemplateNotifier;
use crate::services::plans_flexible_service::PlansFlexibleService;

use super::super::analysis::require_analysis;
use super::super::commit::{commit_state, hand_off};
use super::super::super::placeholder;

/// 子 agent 工具名（日志用，链组装也要用）。
pub(super) const AGENT: &str = "flexible_save";
/// 定稿 status：输出顶层 `status` 等于它才算定稿。
pub(super) const OK_STATUS: &str = "saved";
/// 定稿后推进到的 `current_step` 档位。
const NEXT_STEP: &str = "saved";

/// `flexible_save` 的定稿登记回调。
pub(super) struct SaveCallback {
    /// 该 plan 的 id（plan 级注册时已知，是常量）。
    plan_id: String,
    /// 灵活计划聚合服务（写流程中间状态 + 落库）。
    service: Arc<PlansFlexibleService>,
    /// 模板更新通知句柄：落库成功后通知左侧面板重读模板。
    notifier: TemplateNotifier,
}

impl SaveCallback {
    pub(super) fn new(
        plan_id: String,
        service: Arc<PlansFlexibleService>,
        notifier: TemplateNotifier,
    ) -> Self {
        Self {
            plan_id,
            service,
            notifier,
        }
    }
}

/// 从 `flexible_state.products`（JSON 文本）组装落库 payload：`{ task, inputs, steps, output_schema }`。
///
/// - `task` ← `task_definition.task`（需求澄清定稿产物）
/// - `inputs` ← `inputs`（参数化产出的参数表；缺失视为空表）
/// - `steps` ← `steps`（参数化后的步骤骨架，可变值已写成 `${name}`）
/// - `output_schema` ← `output_schema`（输出定义定稿的输出契约）。**缺失或 `null` 一律落 `null`** ——
///   用户跳过输出定义、或选了「现在还定不了」，都是合法情况，不得报错；非 `null` 时必须是对象。
///
/// 落库前校验 `steps` 里的 `${name}` 都能在 `inputs` 中找到同名定义：未定义即报错，
/// 不静默留空 —— 否则「参数漏定义」会被伪装成「本来就没有参数」。
fn build_payload(products: &str) -> Result<String, String> {
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
    if steps
        .as_array()
        .is_none_or(|items| items.is_empty())
    {
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

    // 占位符校验同时覆盖 `steps` 与 `output_schema` 的文本字段
    placeholder::validate(steps, schema_for_placeholders, &inputs)?;

    Ok(json!({
        "task": task,
        "inputs": inputs,
        "steps": steps,
        "output_schema": output_schema,
    })
    .to_string())
}

#[async_trait]
impl SubAgentResultCallback for SaveCallback {
    async fn on_result(&self, call: &SubAgentCall<'_>) -> ResultDecision {
        tracing::info!(
            "[{}] 子 agent '{}' 完成, tool_call_id={}, content_len={}, is_error={}",
            AGENT,
            call.ctx.agent_name,
            call.ctx.tool_call_id,
            call.text().len(),
            call.result.is_error,
        );

        // ── 取前置分析结论（解析 / 定稿判定 / 会话归属都已由它完成）──
        let analysis = match require_analysis(AGENT, call) {
            Ok(analysis) => analysis,
            Err(decision) => return decision,
        };

        // 读已登记的 task / inputs / steps，组装 payload 后整段落库（不经 LLM 转抄）
        let payload = match self
            .service
            .load_state(&self.plan_id, &analysis.session_id)
            .await
        {
            Ok(Some((_, products))) => match build_payload(&products) {
                Ok(p) => p,
                Err(reason) => {
                    return ResultDecision::Abort(format!("[{AGENT}] 落库取数失败：{reason}"))
                }
            },
            Ok(None) => {
                return ResultDecision::Abort(format!(
                    "[{AGENT}] 会话 {} 尚无流程状态记录",
                    analysis.session_id
                ))
            }
            Err(e) => {
                return ResultDecision::Abort(format!(
                    "[{AGENT}] 读取流程状态失败: {e}"
                ))
            }
        };

        if let Err(e) = self
            .service
            .save_snapshot(&self.plan_id, &analysis.session_id, &payload)
            .await
        {
            return ResultDecision::Abort(format!("[{AGENT}] 落库失败: {e}"));
        }

        // 定稿已落库 → 立即通知左侧面板重读模板。
        // 放在 commit_state 之前：数据本身已变，即便后续推进档位失败，这次刷新也成立。
        self.notifier.notify();

        // 推进 saved（无产物登记，空补丁）
        let patch: Map<String, Value> = Map::new();
        if let Err(reason) = commit_state(
            AGENT,
            &self.service,
            &self.plan_id,
            &analysis.session_id,
            Some(NEXT_STEP),
            &patch,
        )
        .await
        {
            return ResultDecision::Abort(reason);
        }

        hand_off(call)
    }

    fn name(&self) -> &str {
        AGENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本 step 独有的回归价值：从 products 组装四件套（含可选的输出契约），且 `steps` 保持**带占位**的模板形态。
    #[test]
    fn builds_payload_from_task_inputs_and_steps() {
        let products = serde_json::json!({
            "task_definition": { "task": "在 C:/a/b/text.txt 追加一行" },
            "inputs": [
                { "name": "filepath", "default": "C:/a/b/text.txt", "description": "日志路径" }
            ],
            "steps": [
                {
                    "result_reference": "#E1",
                    "intent": "在 ${filepath} 追加一行",
                    "expected_output": "${filepath} 新增一行",
                    "dependencies": [],
                }
            ],
            "output_schema": {
                "kind": "csv",
                "goal": "向 ${filepath} 追加一行并导出清单",
                "success": "文件末尾新增一行即视为成功",
                "format": "UTF-8 CSV，首行表头，列 path",
                "required": ["path"],
                "wanted": [],
            },
        })
        .to_string();

        let payload = build_payload(&products).unwrap();
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["task"], "在 C:/a/b/text.txt 追加一行");
        assert_eq!(v["inputs"][0]["name"], "filepath");
        // 落库的是「模板」：占位符不被展开
        assert_eq!(v["steps"][0]["intent"], "在 ${filepath} 追加一行");
        assert_eq!(v["output_schema"]["kind"], "csv");
        // 落库的是模板：占位符不被展开
        assert_eq!(
            v["output_schema"]["goal"],
            "向 ${filepath} 追加一行并导出清单"
        );
        assert_eq!(v["output_schema"]["required"][0], "path");
        assert!(v.get("parameterized_task").is_none());
    }

    /// 输出契约缺失 / 为 `null` 都合法（用户跳过输出定义或选「定不了」），一律落 `null`；
    /// 非对象则报错（不让垃圾进库）。
    #[test]
    fn output_schema_defaults_to_null_and_rejects_non_object() {
        let base = |extra: &str| {
            format!(
                r##"{{"task_definition":{{"task":"建目录"}},
                     "steps":[{{"result_reference":"#E1","intent":"i","expected_output":"o"}}]{extra}}}"##
            )
        };

        // 缺失
        let payload = build_payload(&base("")).unwrap();
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert!(v["output_schema"].is_null(), "缺失 → null");

        // 显式 null
        let payload = build_payload(&base(r#","output_schema":null"#)).unwrap();
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert!(v["output_schema"].is_null(), "显式 null → null");

        // 非对象
        let err = build_payload(&base(r#","output_schema":"csv""#)).unwrap_err();
        assert!(err.contains("output_schema"), "应点名字段: {err}");
    }

    /// 契约形态不合法要挡在落库前：kind 非法 / bool 缺 success / 契约里自创占位符。
    #[test]
    fn rejects_invalid_schema_before_saving() {
        let base = |extra: &str| {
            format!(
                r##"{{"task_definition":{{"task":"建目录"}},
                     "inputs":[{{"name":"dir","default":"/tmp/demo","description":"目录"}}],
                     "steps":[{{"result_reference":"#E1","intent":"在 ${{dir}} 建目录","expected_output":"o"}}]{extra}}}"##
            )
        };

        // kind 非法（旧值 `success_only` 已改名为 `bool`）
        let err = build_payload(&base(
            r#","output_schema":{"kind":"success_only","success":"s"}"#,
        ))
        .unwrap_err();
        assert!(err.contains("success_only"), "{err}");

        // bool 缺 success
        let err = build_payload(&base(r#","output_schema":{"kind":"bool","goal":"g"}"#))
            .unwrap_err();
        assert!(err.contains("success"), "{err}");

        // 契约里自创占位符：错误要点名 output_schema
        let err = build_payload(&base(
            r#","output_schema":{"kind":"bool","goal":"向 ${gone} 追加","success":"s"}"#,
        ))
        .unwrap_err();
        assert!(err.contains("${gone}"), "{err}");
        assert!(err.contains("output_schema"), "错误应点名来源: {err}");

        // 合法契约照旧通过（契约里的占位符与 steps 用同一个已定义参数）
        assert!(build_payload(&base(
            r#","output_schema":{"kind":"bool","goal":"在 ${dir} 建目录","success":"目录已建"}"#,
        ))
        .is_ok());
    }

    #[test]
    fn missing_or_invalid_pieces_are_errors() {
        assert!(build_payload("not json").is_err());
        assert!(build_payload("[]").is_err());
        assert!(build_payload("{}").is_err());

        let no_task = serde_json::json!({
            "task_definition": {},
            "steps": [{ "intent": "做事" }],
        })
        .to_string();
        assert!(build_payload(&no_task).is_err(), "缺 task 应报错");

        let no_steps = serde_json::json!({
            "task_definition": { "task": "建目录" },
        })
        .to_string();
        assert!(build_payload(&no_steps).is_err(), "缺 steps 应报错");

        let empty_steps = serde_json::json!({
            "task_definition": { "task": "建目录" },
            "steps": [],
        })
        .to_string();
        assert!(build_payload(&empty_steps).is_err(), "空 steps 应报错");
    }

    /// 自创占位符必须在落库前被挡下（静默留空会把「漏定义」伪装成「本就无参数」）。
    #[test]
    fn rejects_undefined_placeholder() {
        let products = serde_json::json!({
            "task_definition": { "task": "建目录" },
            "inputs": [
                { "name": "filepath", "default": "C:/a/b.txt", "description": "路径" }
            ],
            "steps": [
                {
                    "result_reference": "#E1",
                    "intent": "在 ${filepath} 追加 ${out_dir} 下的一行",
                    "expected_output": "写好了",
                    "dependencies": [],
                }
            ],
        })
        .to_string();

        let err = build_payload(&products).unwrap_err();
        assert!(err.contains("${out_dir}"), "错误应点名未定义占位符: {err}");
    }

    /// `inputs` 缺失视为空参数表：`steps` 里没有占位符时仍可落库。
    #[test]
    fn missing_inputs_means_no_parameters() {
        let products = serde_json::json!({
            "task_definition": { "task": "建目录" },
            "steps": [
                {
                    "result_reference": "#E1",
                    "intent": "在 /tmp/demo 建目录",
                    "expected_output": "/tmp/demo 存在",
                    "dependencies": [],
                }
            ],
        })
        .to_string();

        let payload = build_payload(&products).unwrap();
        let v: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["inputs"], serde_json::json!([]));
        assert_eq!(v["steps"][0]["intent"], "在 /tmp/demo 建目录");
        assert!(v["output_schema"].is_null(), "无输出定义 → null");
    }
}
