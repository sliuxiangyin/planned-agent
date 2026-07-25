use std::sync::Arc;
use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};
use planned_agent_core::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{ToolExecutor, ToolCategory, BuiltinToolProvider};

/// 内置文本工具提供者
pub struct TextToolsProvider;

impl BuiltinToolProvider for TextToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![
            (
                Tool {
                    name: "builtin_text_search".to_string(),
                    description: "在文本中搜索关键词（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "text": { "type": "string" },
                            "pattern": { "type": "string" }
                        },
                        "required": ["text", "pattern"]
                    }),
                },
                vec![ToolCategory::Text],
            ),
            (
                Tool {
                    name: "builtin_text_replace".to_string(),
                    description: "替换文本中的内容（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "text": { "type": "string" },
                            "from": { "type": "string" },
                            "to": { "type": "string" }
                        },
                        "required": ["text", "from", "to"]
                    }),
                },
                vec![ToolCategory::Text],
            ),
        ]
    }
    
    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(TextToolsExecutor)
    }
}

struct TextToolsExecutor;

#[async_trait]
impl ToolExecutor for TextToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        match tool_name {
            "builtin_text_search" => {
                let text = arguments["text"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing text"))?;
                let pattern = arguments["pattern"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing pattern"))?;
                
                let matches: Vec<usize> = text.match_indices(pattern)
                    .map(|(i, _)| i)
                    .collect();
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "matches": matches, "count": matches.len() }),
                    is_error: false,
                })
            }
            "builtin_text_replace" => {
                let text = arguments["text"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing text"))?;
                let from = arguments["from"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing from"))?;
                let to = arguments["to"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing to"))?;
                
                let result = text.replace(from, to);
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "result": result }),
                    is_error: false,
                })
            }
            _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name))
        }
    }
    
    fn name(&self) -> &str {
        "builtin_text_tools"
    }
    
    fn supported_tools(&self) -> Vec<String> {
        vec![
            "builtin_text_search".to_string(),
            "builtin_text_replace".to_string(),
        ]
    }
}
