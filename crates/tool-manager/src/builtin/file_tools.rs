use std::sync::Arc;
use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};
use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{ToolExecutor, ToolCategory, BuiltinToolProvider};

/// 内置文件工具提供者
pub struct FileToolsProvider;

impl BuiltinToolProvider for FileToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![
            (
                Tool {
                    name: "builtin_read_file".to_string(),
                    description: "读取文件内容（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
                    }),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_write_file".to_string(),
                    description: "写入文件内容（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["path", "content"]
                    }),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_list_dir".to_string(),
                    description: "列出目录内容（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
                    }),
                },
                vec![ToolCategory::File],
            ),
        ]
    }
    
    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(FileToolsExecutor)
    }
}

/// 文件工具执行器
struct FileToolsExecutor;

#[async_trait]
impl ToolExecutor for FileToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        match tool_name {
            "builtin_read_file" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?;
                let content = std::fs::read_to_string(path)?;
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "content": content }),
                    is_error: false,
                })
            }
            "builtin_write_file" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?;
                let content = arguments["content"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing content"))?;
                std::fs::write(path, content)?;
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "success": true }),
                    is_error: false,
                })
            }
            "builtin_list_dir" => {
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?;
                let entries: Vec<String> = std::fs::read_dir(path)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "entries": entries }),
                    is_error: false,
                })
            }
            _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name))
        }
    }
    
    fn name(&self) -> &str {
        "builtin_file_tools"
    }
    
    fn supported_tools(&self) -> Vec<String> {
        vec![
            "builtin_read_file".to_string(),
            "builtin_write_file".to_string(),
            "builtin_list_dir".to_string(),
        ]
    }
}
