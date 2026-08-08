use anyhow::Result;
use serde_json::Value;
use planned_agent_core::mcp::types::Tool;
use tracing::warn;

/// 工具参数验证器
pub struct ToolValidator;

impl ToolValidator {
    /// 验证工具参数
    pub fn validate_arguments(tool: &Tool, arguments: &Value) -> Result<()> {
        // 检查必需字段
        if let Some(required) = tool.input_schema.get("required") {
            if let Some(required_fields) = required.as_array() {
                for field in required_fields {
                    if let Some(field_name) = field.as_str() {
                        if arguments.get(field_name).is_none() {
                            return Err(anyhow::anyhow!(
                                "Missing required field '{}' for tool '{}'",
                                field_name,
                                tool.name
                            ));
                        }
                    }
                }
            }
        }
        
        // 检查字段类型
        if let Some(properties) = tool.input_schema.get("properties") {
            if let Some(props) = properties.as_object() {
                for (field_name, field_schema) in props {
                    if let Some(field_value) = arguments.get(field_name) {
                        if let Some(expected_type) = field_schema.get("type").and_then(|t| t.as_str()) {
                            let valid = match expected_type {
                                "string" => field_value.is_string(),
                                "number" | "integer" => field_value.is_number(),
                                "boolean" => field_value.is_boolean(),
                                "array" => field_value.is_array(),
                                "object" => field_value.is_object(),
                                _ => true, // 未知类型跳过验证
                            };
                            if !valid {
                                warn!(
                                    "Type mismatch for field '{}' in tool '{}': expected {}",
                                    field_name, tool.name, expected_type
                                );
                            }
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
}
