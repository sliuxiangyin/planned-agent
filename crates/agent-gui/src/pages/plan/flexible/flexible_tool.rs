//! `flexible_tool` —— 灵活模式协调器的两个旁路工具：
//!
//! - `flexible_state`：读写当前会话的「流程中间状态」（当前阶段 + 各步骤产物）。
//!   - `action=load`：读取 `current_step` 与 `products`，供协调器在「用户指定步骤」时盘点
//!     目标步骤的前置产物是否齐备（齐备→直达；缺失→引导）。
//!   - `action=save`：在某一流程阶段「定稿」（如 step1 确认、step2 success、step3/4 用户
//!     确认）后登记该阶段产物，并把 `current_step` 推进到对应档位。
//! - `save_flexible_template`：登记某会话产出的模板快照（由父 agent 在 step5 产出后显式调用）。
//!
//! `session_id` 不再从会话 watch 槽动态读取（多会话并行时会串写），改由协调器从 system
//! prompt 中照抄、作为工具参数传入；executor 从 `arguments.session_id` 读取。`plan_id` 仍由
//! executor 构造时注入（per-plan，天然正确），不依赖协调器传入。

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::ToolExecutor;

use crate::services::plans_flexible_service::PlansFlexibleService;

/// 从工具参数读取 `session_id`；缺失 / 空 / 非字符串时返回可读错误。
///
/// `session_id` 由协调器从 system prompt 中照抄传入（system prompt 已在全局主 agent 中定义）。
fn read_session_id(arguments: &Value) -> std::result::Result<String, String> {
    match arguments.get("session_id").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(
            "缺少 session_id：请原样照抄 system prompt 中本会话的 session ID，不得改写或省略"
                .to_string(),
        ),
    }
}

/// 构造一个 is_error 的 ToolResult。
fn error_result(msg: &str) -> Result<ToolResult> {
    Ok(ToolResult {
        call_id: String::new(),
        content: json!({ "error": msg }),
        is_error: true,
    })
}

// ────────────────────────────── flexible_state ──────────────────────────────

/// `flexible_state` 执行器：读/写当前会话的流程中间状态。
///
/// `session_id` 由协调器经 `arguments.session_id` 传入（不再绑定会话 watch 槽）。
pub struct FlexibleStateExecutor {
    plan_id: String,
    service: Arc<PlansFlexibleService>,
}

impl FlexibleStateExecutor {
    pub fn new(plan_id: String, service: Arc<PlansFlexibleService>) -> Self {
        Self { plan_id, service }
    }
}

#[async_trait]
impl ToolExecutor for FlexibleStateExecutor {
    async fn execute(&self, _tool_name: &str, arguments: Value) -> Result<ToolResult> {
        let session_id = match read_session_id(&arguments) {
            Ok(s) => s,
            Err(msg) => return error_result(&msg),
        };
        let plan_id = self.plan_id.clone();

        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let result = match action.as_str() {
            "load" => self.load(&plan_id, &session_id).await,
            "save" => self.save(&plan_id, &session_id, &arguments).await,
            other => {
                let msg = format!(
                    "未知 action '{}'：仅支持 load（读取当前流程状态）或 save（登记某阶段产物）。",
                    other
                );
                return error_result(&msg);
            }
        };

        match result {
            Ok(content) => Ok(ToolResult {
                call_id: String::new(),
                content: Value::String(content),
                is_error: false,
            }),
            Err(e) => error_result(&format!("读写流程状态失败: {}", e)),
        }
    }

    fn name(&self) -> &str {
        "FlexibleStateExecutor"
    }

    fn description(&self) -> &str {
        "Read/write the flexible-flow intermediate state (current_step + step products) for the current session"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec!["flexible_state".into()]
    }
}

impl FlexibleStateExecutor {
    /// load：返回 `{ loaded, current_step, products }`（products 为对象；无记录时 current_step="none"）。
    async fn load(&self, plan_id: &str, session_id: &str) -> Result<String> {
        let state = self.service.load_state(plan_id, session_id).await?;
        match state {
            Some((current_step, products)) => {
                let products_val: Value = serde_json::from_str(&products)
                    .unwrap_or_else(|_| Value::Object(Default::default()));
                Ok(json!({
                    "loaded": true,
                    "current_step": current_step,
                    "products": products_val
                })
                .to_string())
            }
            None => Ok(json!({
                "loaded": false,
                "current_step": "none",
                "products": {}
            })
            .to_string()),
        }
    }

    /// save：登记某阶段的产物。`current_step` 与 `products`（对象）均可选；未提供则保留原值。
    /// 读-改-写合并：先读当前，再合并传入字段后覆盖。
    async fn save(&self, plan_id: &str, session_id: &str, args: &Value) -> Result<String> {
        let existing = self.service.load_state(plan_id, session_id).await?;
        let (mut current_step, mut products_obj) = match existing {
            Some((step, products)) => {
                let parsed: Value = serde_json::from_str(&products)
                    .unwrap_or_else(|_| Value::Object(Default::default()));
                (step, parsed)
            }
            None => ("none".to_string(), Value::Object(Default::default())),
        };

        if let Some(step) = args.get("current_step").and_then(Value::as_str) {
            current_step = step.to_string();
        }
        if let Some(Value::Object(map)) = args.get("products") {
            if let Value::Object(existing_map) = &mut products_obj {
                for (k, v) in map {
                    if v.is_null() {
                        existing_map.remove(k);
                    } else {
                        existing_map.insert(k.clone(), v.clone());
                    }
                }
            } else {
                products_obj = Value::Object(map.clone());
            }
        }

        let products_str = serde_json::to_string(&products_obj)?;
        let saved = self
            .service
            .save_state(plan_id, session_id, &current_step, &products_str)
            .await?;
        Ok(json!({
            "saved": true,
            "current_step": saved.0,
            "products": serde_json::from_str::<Value>(&saved.1).unwrap_or(Value::Object(Default::default()))
        })
        .to_string())
    }
}

/// 构造 `flexible_state` 工具定义。
pub fn flexible_state_tool() -> Tool {
    Tool {
        name: "flexible_state".into(),
        description: "读写当前会话的灵活模式流程状态。\n\
             \n\
             用途：协调器在用户要求「直接执行 / 跳到某一步骤」时，先 load 当前已推进到哪一步、\n\
             各步骤产物是否齐备，据此前置判断；在每步「定稿」后（如 step1 需求确认、step2 执行成功、\n\
             step3 字段确认、step4 参数确认）调用 save 登记产物并推进阶段。\n\
             \n\
             session_id（必填）：本会话的 session ID，原样照抄 system prompt「会话上下文」中给出的值，\n\
             不得改写、不得省略。\n\
             \n\
             action：\n\
             - load：返回该会话当前的 { current_step, products }，无记录时 current_step=none。\n\
             - save：登记某阶段产物。参数 current_step（string，如 task_defined / executed /\n\
               fields_selected / params_confirmed）与 products（object，key 见下）可选；\n\
               未提供的字段保留原值，product 传 null 表示清除该项。\n\
             \n\
             products 的 key（均为 string，存原始文本/JSON）：task_definition、output_format、\n\
             execution_trace、compressed_context、field_selection_result、parameter_confirmation_result。\n\
             \n\
             current_step 档位（顺序 none→task_defined→executed→fields_selected→params_confirmed→templated）：\n\
             - task_defined    = step1 已确认（可执行 step2）\n\
             - executed        = step2 已成功（可做 step3 字段选择）\n\
             - fields_selected = step3 已确认（可做 step4 参数确认）\n\
             - params_confirmed= step4 已确认（可编译 step5 模板）\n\
             - templated       = step5 已定稿".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "本会话的 session ID，原样照抄 system prompt 中给出的值"
                },
                "action": {
                    "type": "string",
                    "enum": ["load", "save"],
                    "description": "load=读取当前流程状态；save=登记某阶段产物并推进 current_step"
                },
                "current_step": {
                    "type": "string",
                    "description": "(仅 save) 推进到的阶段档位：task_defined / executed / fields_selected / params_confirmed / templated"
                },
                "products": {
                    "type": "object",
                    "description": "(仅 save) 要写入的产物；key 为 task_definition / output_format / execution_trace / compressed_context / field_selection_result / parameter_confirmation_result，值传 null 表示清除"
                }
            },
            "required": ["session_id", "action"]
        }),
    }
}

// ────────────────────────── save_flexible_template ──────────────────────────

/// `save_flexible_template` 执行器：把某会话 step5 产出的模板快照落库。
///
/// `session_id` 由协调器经 `arguments.session_id` 传入（不再绑定会话 watch 槽）；
/// `template` 为 step5 返回的完整模板 JSON。原 `step5_callback` 的 JSON 结构校验迁移至此，
/// 校验失败返回 `is_error`，由父 agent 决定重跑 step5 或重传。
pub struct SaveTemplateExecutor {
    plan_id: String,
    service: Arc<PlansFlexibleService>,
}

impl SaveTemplateExecutor {
    pub fn new(plan_id: String, service: Arc<PlansFlexibleService>) -> Self {
        Self { plan_id, service }
    }
}

#[async_trait]
impl ToolExecutor for SaveTemplateExecutor {
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
        "SaveTemplateExecutor"
    }

    fn description(&self) -> &str {
        "Persist the flexible-flow template snapshot (from step5 output) for a session"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec!["save_flexible_template".into()]
    }
}

/// 从 JSON 中取指定字段；缺失时回退到 `default`（作为字符串存储）。
fn take_or_default(json: &Value, key: &str, default: &str) -> String {
    json.get(key)
        .map(Value::to_string)
        .unwrap_or_else(|| default.to_string())
}

/// 构造 `save_flexible_template` 工具定义。
pub fn save_flexible_template() -> Tool {
    Tool {
        name: "save_flexible_template".into(),
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
