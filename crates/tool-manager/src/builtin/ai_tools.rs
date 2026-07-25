use std::sync::Arc;
use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};
use planned_agent_core::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{ToolExecutor, ToolCategory, BuiltinToolProvider};

/// 内置AI处理工具提供者
pub struct AiToolsProvider;

impl BuiltinToolProvider for AiToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![
            (
                Tool {
                    name: "ai_process".to_string(),
                    description: "使用AI直接处理数据（适合分析、提取、转换、计算等任务）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "data": {
                                "description": "待处理的数据（可以是任意类型）"
                            },
                            "instruction": {
                                "type": "string",
                                "description": "处理指令，描述需要对数据做什么处理"
                            }
                        },
                        "required": ["data", "instruction"]
                    }),
                },
                vec![ToolCategory::Data],
            ),
        ]
    }
    
    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(AiToolsExecutor)
    }
}

/// AI处理工具执行器
/// 注意：这个工具的实际执行由 ReAct Agent 处理，这里只是占位
struct AiToolsExecutor;

#[async_trait]
impl ToolExecutor for AiToolsExecutor {
    async fn execute(&self, _tool_name: &str, _arguments: Value) -> Result<ToolResult> {
        // 这个工具不应该被直接调用
        // 实际执行由 ReAct Agent 的 execute_tool 方法特殊处理
        Err(anyhow::anyhow!("ai_process tool should be handled by ReAct Agent directly"))
    }
    
    fn name(&self) -> &str {
        "builtin_ai_tools"
    }
    
    fn supported_tools(&self) -> Vec<String> {
        vec![
            "ai_process".to_string(),
        ]
    }
}
