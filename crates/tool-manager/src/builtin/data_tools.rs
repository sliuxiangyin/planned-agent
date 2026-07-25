use std::sync::Arc;
use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};
use planned_agent_core::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{ToolExecutor, ToolCategory, BuiltinToolProvider};

/// 内置数据处理工具提供者
pub struct DataToolsProvider;

impl BuiltinToolProvider for DataToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![
            (
                Tool {
                    name: "builtin_sort_data".to_string(),
                    description: "对数据进行排序（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "data": { 
                                "type": "array",
                                "description": "待排序的数据数组"
                            },
                            "key": { 
                                "type": "string",
                                "description": "排序字段（可选，用于对象数组）"
                            },
                            "reverse": { 
                                "type": "boolean",
                                "description": "是否降序排序",
                                "default": false
                            }
                        },
                        "required": ["data"]
                    }),
                },
                vec![ToolCategory::Data],
            ),
            (
                Tool {
                    name: "builtin_filter_data".to_string(),
                    description: "过滤数据（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "data": { 
                                "type": "array",
                                "description": "待过滤的数据数组"
                            },
                            "condition": { 
                                "type": "object",
                                "description": "过滤条件"
                            }
                        },
                        "required": ["data", "condition"]
                    }),
                },
                vec![ToolCategory::Data],
            ),
            (
                Tool {
                    name: "builtin_extract_data".to_string(),
                    description: "从数据中提取特定字段或元素（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "data": { 
                                "type": ["array", "object"],
                                "description": "源数据"
                            },
                            "path": { 
                                "type": "string",
                                "description": "提取路径，如 '0'、'name'、'items[0]'"
                            },
                            "count": { 
                                "type": "integer",
                                "description": "提取数量（可选）",
                                "default": 1
                            }
                        },
                        "required": ["data", "path"]
                    }),
                },
                vec![ToolCategory::Data],
            ),
            (
                Tool {
                    name: "builtin_aggregate_data".to_string(),
                    description: "对数据进行聚合计算（内置工具）".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "data": { 
                                "type": "array",
                                "description": "待聚合的数据数组"
                            },
                            "operation": { 
                                "type": "string",
                                "enum": ["sum", "avg", "min", "max", "count"],
                                "description": "聚合操作"
                            },
                            "field": { 
                                "type": "string",
                                "description": "聚合字段（可选，用于对象数组）"
                            }
                        },
                        "required": ["data", "operation"]
                    }),
                },
                vec![ToolCategory::Data],
            ),
        ]
    }
    
    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(DataToolsExecutor)
    }
}

struct DataToolsExecutor;

#[async_trait]
impl ToolExecutor for DataToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        match tool_name {
            "builtin_sort_data" => {
                let data = arguments["data"].as_array()
                    .ok_or_else(|| anyhow::anyhow!("Missing data array"))?;
                let key = arguments["key"].as_str();
                let reverse = arguments["reverse"].as_bool().unwrap_or(false);
                
                let mut sorted_data = data.clone();
                
                if let Some(key) = key {
                    // 按对象字段排序
                    sorted_data.sort_by(|a, b| {
                        let a_val = a.get(key).unwrap_or(&Value::Null);
                        let b_val = b.get(key).unwrap_or(&Value::Null);
                        
                        let ordering = match (a_val, b_val) {
                            (Value::Number(a), Value::Number(b)) => {
                                a.as_f64().unwrap_or(0.0).partial_cmp(&b.as_f64().unwrap_or(0.0))
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            }
                            (Value::String(a), Value::String(b)) => a.cmp(b),
                            _ => std::cmp::Ordering::Equal,
                        };
                        
                        if reverse { ordering.reverse() } else { ordering }
                    });
                } else {
                    // 直接排序
                    sorted_data.sort_by(|a, b| {
                        let ordering = match (a, b) {
                            (Value::Number(a), Value::Number(b)) => {
                                a.as_f64().unwrap_or(0.0).partial_cmp(&b.as_f64().unwrap_or(0.0))
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            }
                            (Value::String(a), Value::String(b)) => a.cmp(b),
                            _ => std::cmp::Ordering::Equal,
                        };
                        
                        if reverse { ordering.reverse() } else { ordering }
                    });
                }
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "sorted_data": sorted_data }),
                    is_error: false,
                })
            }
            "builtin_filter_data" => {
                let data = arguments["data"].as_array()
                    .ok_or_else(|| anyhow::anyhow!("Missing data array"))?;
                let condition = arguments["condition"].as_object()
                    .ok_or_else(|| anyhow::anyhow!("Missing condition object"))?;
                
                let filtered_data: Vec<Value> = data.iter()
                    .filter(|item| {
                        // 简单的条件匹配
                        for (key, expected) in condition {
                            if let Some(actual) = item.get(key) {
                                if actual != expected {
                                    return false;
                                }
                            } else {
                                return false;
                            }
                        }
                        true
                    })
                    .cloned()
                    .collect();
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "filtered_data": filtered_data, "count": filtered_data.len() }),
                    is_error: false,
                })
            }
            "builtin_extract_data" => {
                let data = &arguments["data"];
                let path = arguments["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?;
                let count = arguments["count"].as_i64().unwrap_or(1) as usize;
                
                let mut result = Vec::new();
                
                if let Some(array) = data.as_array() {
                    // 从数组中提取
                    if let Ok(index) = path.parse::<usize>() {
                        if index < array.len() {
                            result.push(array[index].clone());
                        }
                    }
                } else if let Some(object) = data.as_object() {
                    // 从对象中提取
                    if let Some(value) = object.get(path) {
                        result.push(value.clone());
                    }
                }
                
                // 限制数量
                result.truncate(count);
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "extracted": result, "count": result.len() }),
                    is_error: false,
                })
            }
            "builtin_aggregate_data" => {
                let data = arguments["data"].as_array()
                    .ok_or_else(|| anyhow::anyhow!("Missing data array"))?;
                let operation = arguments["operation"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing operation"))?;
                let field = arguments["field"].as_str();
                
                let values: Vec<f64> = data.iter()
                    .filter_map(|item| {
                        let value = if let Some(field) = field {
                            item.get(field)?
                        } else {
                            item
                        };
                        value.as_f64()
                    })
                    .collect();
                
                let result = match operation {
                    "sum" => json!(values.iter().sum::<f64>()),
                    "avg" => json!(if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 }),
                    "min" => json!(values.iter().cloned().fold(f64::INFINITY, f64::min)),
                    "max" => json!(values.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
                    "count" => json!(values.len()),
                    _ => return Err(anyhow::anyhow!("Unknown operation: {}", operation)),
                };
                
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({ "result": result, "operation": operation, "count": values.len() }),
                    is_error: false,
                })
            }
            _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name))
        }
    }
    
    fn name(&self) -> &str {
        "builtin_data_tools"
    }
    
    fn supported_tools(&self) -> Vec<String> {
        vec![
            "builtin_sort_data".to_string(),
            "builtin_filter_data".to_string(),
            "builtin_extract_data".to_string(),
            "builtin_aggregate_data".to_string(),
        ]
    }
}
