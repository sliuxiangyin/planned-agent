//! 步骤结果查询工具（内置）
//!
//! 提供 `fetch_step_result` 工具，通过 schema 参数传入步骤结果映射，
//! 按 reference 查找对应输出。无状态，纯数据驱动。

use std::sync::Arc;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::tool_registry::{ToolCategory, ToolExecutor, BuiltinToolProvider};
use planned_agent_core::types::{Tool, ToolResult};

pub fn fetch_step_result_tool() -> (Tool, Vec<ToolCategory>) {
    (
        Tool {
            name: "builtin_fetch_step_result".to_string(),
            description: "获取前序步骤的执行结果。传入步骤的引用标识（如 #E1、#E2），返回该步骤的原始输出。"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "reference": {
                        "type": "string",
                        "description": "步骤的结果引用标识，如 #E1、#E2"
                    }
                },
                "required": ["reference"]
            }),
        },
       vec![ToolCategory::Utility],
    )
}

/// 无状态执行器：从 schema 传入的 results map 中按 reference 查找。
pub struct FetchStepResultExecutor;

#[async_trait]
impl ToolExecutor for FetchStepResultExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        if tool_name != "builtin_fetch_step_result" {
            return Err(anyhow::anyhow!("Unknown tool: {}", tool_name));
        }

        let reference = arguments["reference"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("缺少 reference 参数"))?;

        let results = arguments["results"]
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("缺少 results 参数或格式错误（应为 object）"))?;

        match results.get(reference) {
            Some(output) => Ok(ToolResult {
                call_id: uuid::Uuid::new_v4().to_string(),
                content: json!({
                    "reference": reference,
                    "output": output,
                }),
                is_error: false,
            }),
            None => Ok(ToolResult {
                call_id: uuid::Uuid::new_v4().to_string(),
                content: json!({
                    "reference": reference,
                    "error": format!("未找到引用标识为 {} 的步骤结果。可用引用: {:?}",
                        reference, results.keys().collect::<Vec<_>>())
                }),
                is_error: true,
            }),
        }
    }

    fn name(&self) -> &str {
        "fetch_step_result_executor"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec!["builtin_fetch_step_result".to_string()]
    }
}

/// 内置步骤结果工具提供者
pub struct StepResultToolsProvider;

impl BuiltinToolProvider for StepResultToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![fetch_step_result_tool()]
    }

    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(FetchStepResultExecutor)
    }
}
