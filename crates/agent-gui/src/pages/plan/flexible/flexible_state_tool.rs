//! `flexible_state` 工具 —— 协调器读写当前会话的「流程中间状态」（当前阶段 + 各步骤产物）。
//!
//! 作用：
//! - `action=load`：读取当前会话的 `current_step` 与 `products`，供协调器在「用户指定
//!   步骤」时盘点目标步骤的前置产物是否齐备（齐备→直达；缺失→引导）。
//! - `action=save`：在某一流程阶段「定稿」（如 step1 确认、step2 success、step3/4 用户
//!   确认）后登记该阶段产物，并把 `current_step` 推进到对应档位。
//!
//! `session_id` 由 executor 从当前会话 watch 槽（与 step5_callback 同源）动态读取，
//! 天然跟随会话切换；不依赖协调器传入 plan/session。

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::watch;

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::ToolExecutor;

use crate::services::plans_flexible_service::PlansFlexibleService;

/// `flexible_state` 执行器：读/写当前会话的流程中间状态。
pub struct FlexibleStateExecutor {
    plan_id: String,
    service: Arc<PlansFlexibleService>,
    /// 当前会话 watch receiver：execute 时 `borrow()` 读当前 session 定位归属。
    session_rx: watch::Receiver<Option<String>>,
}

impl FlexibleStateExecutor {
    pub fn new(
        plan_id: String,
        service: Arc<PlansFlexibleService>,
        session_rx: watch::Receiver<Option<String>>,
    ) -> Self {
        Self {
            plan_id,
            service,
            session_rx,
        }
    }
}

#[async_trait]
impl ToolExecutor for FlexibleStateExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        let plan_id = self.plan_id.clone();
        let session_id = self.session_rx.borrow().clone();
        let Some(session_id) = session_id else {
            // 不应到达：协调器只在 ChatService 就绪后运行，此时 controller 必已写入会话槽。
            return self.error(
                tool_name,
                "当前会话 id 缺失（会话时序/注册不变量被破坏），无法读写流程状态",
            );
        };

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
                return self.error(tool_name, &msg);
            }
        };

        match result {
            Ok(content) => Ok(ToolResult {
                call_id: String::new(),
                content: Value::String(content),
                is_error: false,
            }),
            Err(e) => self.error(tool_name, &format!("读写流程状态失败: {}", e)),
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

    /// 构造一个 is_error 的 ToolResult。
    fn error(&self, _tool_name: &str, msg: &str) -> Result<ToolResult> {
        Ok(ToolResult {
            call_id: String::new(),
            content: json!({ "error": msg }),
            is_error: true,
        })
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
                "required": ["action"]
            }),
        }
}
