//! plans_flexible 自定义工具注册器
//!
//! 对外暴露 `plans_flexible` 工具，让 LLM 通过 ToolRegistry 写入 plans_flexible
//! 版本快照、或按 plan_id 查询最新版本号。由 `app()` 在 Storage 就绪后延后注册。

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{ToolCategory, ToolExecutor};

use crate::services::plans_flexible_service::PlansFlexibleService;

use super::ToolsContext;

/// plans_flexible 工具名（与 `supported_tools` 保持一致）
pub const PLANS_FLEXIBLE_TOOL: &str = "plans_flexible";

/// 注册 `plans_flexible` 自定义工具到 ToolRegistry。
///
/// 透传 `ToolsContext::register_custom_tool`；已存在同名工具时静默覆盖。
pub fn register_plans_flexible_tool(
    tools_ctx: &ToolsContext,
    service: Arc<PlansFlexibleService>,
) {
    tools_ctx.register_custom_tool(
        plans_flexible_tool_definition(),
        vec![ToolCategory::Utility],
        Arc::new(PlansFlexibleExecutor::new(service)),
    );
    tracing::info!("已注册自定义工具: {}", PLANS_FLEXIBLE_TOOL);
}

/// 移除 `plans_flexible` 自定义工具（不存在时返回 Err）。
pub fn unregister_plans_flexible_tool(tools_ctx: &ToolsContext) -> anyhow::Result<()> {
    tools_ctx.unregister_custom_tool(PLANS_FLEXIBLE_TOOL)
}

/// plans_flexible 工具执行器：按 `arguments.operation` 分派到 service。
struct PlansFlexibleExecutor {
    service: Arc<PlansFlexibleService>,
}

impl PlansFlexibleExecutor {
    fn new(service: Arc<PlansFlexibleService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ToolExecutor for PlansFlexibleExecutor {
    async fn execute(&self, _tool_name: &str, arguments: Value) -> Result<ToolResult> {
        let operation = arguments
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("缺少 operation 参数（write / get_latest_version）"))?;

        let result = match operation {
            "write" => {
                let plan_id = arguments
                    .get("plan_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("write 操作缺少 plan_id 参数"))?;
                let default_obj = json!("{}");
                let default_arr = json!("[]");
                let input_schema = arguments.get("input_schema").unwrap_or(&default_obj);
                let output = arguments.get("output").unwrap_or(&default_obj);
                let steps = arguments.get("steps").unwrap_or(&default_arr);
                let execution_plan = arguments.get("execution_plan").unwrap_or(&default_arr);
                let model = self
                    .service
                    .write(
                        plan_id,
                        None, // 工具无会话上下文；会话归属由 step5 落库路径填写
                        &input_schema.to_string(),
                        &output.to_string(),
                        &steps.to_string(),
                        &execution_plan.to_string(),
                    )
                    .await?;
                model_to_value(&model)
            }
            "get_latest_version" => {
                let plan_id = arguments
                    .get("plan_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("get_latest_version 操作缺少 plan_id 参数"))?;
                match self.service.get_latest_version(plan_id).await? {
                    Some(version) => json!({
                        "plan_id": plan_id,
                        "latest_version": version,
                        "found": true,
                    }),
                    None => json!({
                        "plan_id": plan_id,
                        "found": false,
                    }),
                }
            }
            other => return Err(anyhow!("未知 operation: {}", other)),
        };

        Ok(ToolResult {
            call_id: String::new(),
            content: result,
            is_error: false,
        })
    }

    fn name(&self) -> &str {
        "PlansFlexibleExecutor"
    }

    fn description(&self) -> &str {
        "plans_flexible 版本快照的写入与最新版本号查询执行器"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec![PLANS_FLEXIBLE_TOOL.into()]
    }
}

/// 构造 `plans_flexible` 工具定义（JSON Schema）
fn plans_flexible_tool_definition() -> Tool {
    Tool {
        name: PLANS_FLEXIBLE_TOOL.into(),
        description:
            "plans_flexible 计划版本快照。operation 取值：\
             \n- write：写入一条快照，需传 plan_id，可选 input_schema、output、steps、execution_plan（JSON 对象/数组）；version 由系统自动递增（该计划最新版本 +1，首条为 1）。\
             \n- get_latest_version：按 plan_id 获取该计划最新版本的版本号，需传 plan_id。"
                .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["write", "get_latest_version"],
                    "description": "要执行的操作"
                },
                "plan_id": {
                    "type": "string",
                    "description": "write / get_latest_version 操作所属的计划 id"
                },
                "input_schema": { "type": "object", "description": "输入参数定义 JSON（可选）" },
                "output": { "type": "object", "description": "输出定义 JSON（可选）" },
                "steps": { "type": "array", "description": "执行脚本 JSON 数组（可选）" },
                "execution_plan": { "type": "array", "description": "动态修复说明书 JSON 数组（可选）" }
            },
            "required": ["operation"]
        }),
    }
}

/// 把 plans_flexible Model 序列化为 JSON 值（字段均为字符串/i32，直接展开）。
fn model_to_value(model: &crate::storage::entities::plans_flexible::Model) -> Value {
    json!({
        "id": model.id,
        "plan_id": model.plan_id,
        "version": model.version,
        "input_schema": model.input_schema,
        "output": model.output,
        "steps": model.steps,
        "execution_plan": model.execution_plan,
        "created_at": model.created_at,
    })
}
