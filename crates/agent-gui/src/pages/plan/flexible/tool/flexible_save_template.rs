//! `flexible_save_template` 工具：登记某会话产出的模板快照。

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::ToolExecutor;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::{error_result, read_session_id};

/// `flexible_save_template` 执行器：把某会话 step5 产出的模板快照落库。
///
/// `session_id` 由协调器经 `arguments.session_id` 传入（不再绑定会话 watch 槽）；
/// `template` 为 step5 返回的完整模板 JSON。原 `step5_callback` 的 JSON 结构校验迁移至此，
/// 校验失败返回 `is_error`，由父 agent 决定重跑 step5 或重传。
pub struct FlexibleSaveTemplateExecutor {
    plan_id: String,
    service: Arc<PlansFlexibleService>,
}

impl FlexibleSaveTemplateExecutor {
    pub fn new(plan_id: String, service: Arc<PlansFlexibleService>) -> Self {
        Self { plan_id, service }
    }
}

#[async_trait]
impl ToolExecutor for FlexibleSaveTemplateExecutor {
    async fn execute(&self, _tool_name: &str, arguments: Value) -> Result<ToolResult> {
        let session_id = match read_session_id(&arguments) {
            Ok(s) => s,
            Err(msg) => return error_result(&msg),
        };

        // step5 结果产出：完整模板 JSON
        let Some(template) = arguments.get("template") else {
            return error_result(
                "缺少 template：请把 step5 返回的完整模板 JSON（含 input_schema、output、steps、execution_plan、metadata）作为 template 传入。",
            );
        };
        if !template.is_object() {
            return error_result("template 必须是 JSON 对象（step5 返回的完整模板输出）。");
        }
        let json = template;

        // step5 异常分支可能输出 {"status":"error",...}
        if json.get("status").and_then(Value::as_str) == Some("error") {
            return error_result(
                "模板 status 为 error：缺少必要输入数据。请基于 task_definition、execution_trace、field_selection_result、parameter_confirmation_result 补齐后再生成完整模板 JSON。",
            );
        }

        // 校验 steps 与 execution_plan 必须存在、均为数组、长度一致且 step_id 一一对应
        let steps_ok = matches!(json.get("steps"), Some(Value::Array(_)));
        let plan_ok = matches!(json.get("execution_plan"), Some(Value::Array(_)));
        if !steps_ok || !plan_ok {
            return error_result(
                "模板缺少 steps 或 execution_plan 数组字段。请严格输出包含 input_schema、output、steps、execution_plan、metadata 五个顶层字段的完整模板 JSON。",
            );
        }
        let steps_arr = json["steps"].as_array().unwrap();
        let plan_arr = json["execution_plan"].as_array().unwrap();
        let mismatched = steps_arr.len() != plan_arr.len()
            || steps_arr.iter().zip(plan_arr.iter()).any(|(s, p)| {
                s.get("id").and_then(Value::as_str) != p.get("step_id").and_then(Value::as_str)
            });
        if mismatched {
            return error_result(
                "steps 与 execution_plan 必须长度相同且 step_id 一一对应（steps[i].id 必须等于 execution_plan[i].step_id）。请修正后重新生成。",
            );
        }

        let input_schema = take_or_default(json, "input_schema", "{}");
        let output = take_or_default(json, "output", "{}");
        let steps = take_or_default(json, "steps", "[]");
        let execution_plan = take_or_default(json, "execution_plan", "[]");

        match self
            .service
            .save_snapshot(
                &self.plan_id,
                &session_id,
                &input_schema,
                &output,
                &steps,
                &execution_plan,
            )
            .await
        {
            Ok(model) => Ok(ToolResult {
                call_id: String::new(),
                content: Value::String(
                    json!({
                        "saved": true,
                        "id": model.id,
                        "plan_id": model.plan_id,
                        "version": model.version
                    })
                    .to_string(),
                ),
                is_error: false,
            }),
            Err(e) => error_result(&format!("保存模板快照失败: {}", e)),
        }
    }

    fn name(&self) -> &str {
        "FlexibleSaveTemplateExecutor"
    }

    fn description(&self) -> &str {
        "Persist the flexible-flow template snapshot (from step5 output) for a session"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec!["flexible_save_template".into()]
    }
}

/// 从 JSON 中取指定字段；缺失时回退到 `default`（作为字符串存储）。
fn take_or_default(json: &Value, key: &str, default: &str) -> String {
    json.get(key)
        .map(Value::to_string)
        .unwrap_or_else(|| default.to_string())
}

/// 构造 `flexible_save_template` 工具定义。
pub fn flexible_save_template() -> Tool {
    Tool {
        name: "flexible_save_template".into(),
        description: "保存某会话产出的模板快照到 plans_flexible（由父 agent 在 step5 返回 JSON 后显式调用）。\n\
             \n\
             用途：step5 子 agent 返回完整模板 JSON 后，协调器调用本工具把该模板按会话落库；\n\
             同一会话反复产出会覆盖同一版本行，新会话首次产出则新增一条。\n\
             \n\
             session_id（必填）：原样照抄 system prompt「会话上下文」中给出的本会话 session ID。\n\
             template（必填）：step5 返回的完整模板 JSON 对象（含 input_schema、output、steps、\n\
             execution_plan、metadata 五个顶层字段）。\n\
             \n\
             校验失败（非对象 / status=error / steps 与 execution_plan 缺失或 step_id 不一一对应）\n\
             会返回 error，此时请重跑 step5 或修正后重新传入。".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "本会话的 session ID，原样照抄 system prompt 中给出的值"
                },
                "template": {
                    "type": "object",
                    "description": "step5 返回的完整模板 JSON（含 input_schema、output、steps、execution_plan、metadata）"
                }
            },
            "required": ["session_id", "template"]
        }),
    }
}
