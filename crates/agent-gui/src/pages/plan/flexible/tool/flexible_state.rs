//! `flexible_state` 工具：读写当前会话的灵活模式「流程中间状态」。

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::ToolExecutor;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::{error_result, read_session_id};

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
