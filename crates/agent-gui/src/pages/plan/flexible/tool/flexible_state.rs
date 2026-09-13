//! `flexible_state` 工具：读取当前会话的灵活模式「流程中间状态」（只读）。

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::ToolExecutor;

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::{error_result, read_session_id};

/// `flexible_state` 执行器：读取当前会话的流程中间状态（只读）。
///
/// 状态由各 step 子 agent 的完成回调登记（见 `step_callback/`），协调器只读不写，
/// 故本工具不再提供写入能力。`session_id` 由协调器经 `arguments.session_id` 传入。
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
        match self.load(&self.plan_id, &session_id).await {
            Ok(content) => Ok(ToolResult {
                call_id: String::new(),
                content: Value::String(content),
                is_error: false,
            }),
            Err(e) => error_result(&format!("读取流程状态失败: {}", e)),
        }
    }

    fn name(&self) -> &str {
        "FlexibleStateExecutor"
    }

    fn description(&self) -> &str {
        "Read the flexible-flow intermediate state (current_step + step products) for the current session"
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

}

/// 构造 `flexible_state` 工具定义。
pub fn flexible_state_tool() -> Tool {
    Tool {
        name: "flexible_state".into(),
        description: "读取当前会话的灵活模式流程状态（只读）。\n\
             \n\
             用途：协调器在用户要求「直接执行 / 跳到某一步骤」时，读取当前已推进到哪一步、\n\
             各步骤产物是否齐备，据此前置判断；也在需求澄清前读取「任务基线」。\n\
             状态（current_step + products）由各 step 子 agent 的完成回调自动登记，本工具只读、不写入。\n\
             \n\
             session_id（必填）：本会话的 session ID，原样照抄 system prompt「会话上下文」中给出的值，\n\
             不得改写、不得省略。\n\
             \n\
             返回：{ loaded, current_step, products }；该会话尚无记录时 loaded=false、current_step=none、products={}。\n\
             \n\
             products 的 key（均为 string，存原始文本/JSON）：task_definition、output_format、\n\
             execution_trace、compressed_context、field_selection_result、parameter_confirmation_result。\n\
             \n\
             current_step 档位（顺序 none→task_defined→executed→fields_selected→params_confirmed→templated）：\n\
             - task_defined    = step1 已澄清（可执行 step2）\n\
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
                }
            },
            "required": ["session_id"]
        }),
    }
}
