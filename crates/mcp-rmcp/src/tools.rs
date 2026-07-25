use std::collections::HashMap;
use planned_agent_core::types::Tool;
use serde_json::Value;
use anyhow::Result;

/// 工具管理器（支持多服务器）
pub struct ToolManager {
    /// 服务器名称 -> 工具列表
    server_tools: HashMap<String, Vec<Tool>>,
    /// 所有工具的扁平列表
    all_tools: Vec<Tool>,
}

impl ToolManager {
    /// 创建新的工具管理器
    pub fn new() -> Self {
        Self {
            server_tools: HashMap::new(),
            all_tools: Vec::new(),
        }
    }
    
    /// 添加服务器的工具
    pub fn add_tools(&mut self, server_name: &str, tools: Vec<Tool>) {
        self.server_tools.insert(server_name.to_string(), tools);
        self.rebuild_all_tools();
    }
    
    /// 移除服务器的工具
    pub fn remove_server_tools(&mut self, server_name: &str) {
        self.server_tools.remove(server_name);
        self.rebuild_all_tools();
    }
    
    /// 重建所有工具列表
    fn rebuild_all_tools(&mut self) {
        self.all_tools.clear();
        for tools in self.server_tools.values() {
            self.all_tools.extend(tools.clone());
        }
    }
    
    /// 获取所有工具
    pub fn get_all_tools(&self) -> Vec<Tool> {
        self.all_tools.clone()
    }
    
    /// 获取指定服务器的工具
    pub fn get_server_tools(&self, server_name: &str) -> Vec<Tool> {
        self.server_tools.get(server_name).cloned().unwrap_or_default()
    }
    
    /// 根据名称查找工具（在所有服务器中）
    pub fn find_tool(&self, name: &str) -> Option<&Tool> {
        self.all_tools.iter().find(|t| t.name == name)
    }
    
    /// 查找工具所在的服务器
    pub fn find_server_for_tool(&self, tool_name: &str) -> Option<String> {
        for (server_name, tools) in &self.server_tools {
            if tools.iter().any(|t| t.name == tool_name) {
                return Some(server_name.clone());
            }
        }
        None
    }
    
    /// 获取所有服务器名称
    pub fn get_server_names(&self) -> Vec<String> {
        self.server_tools.keys().cloned().collect()
    }
    
    /// 转换工具为 OpenAI 函数格式
    pub fn to_openai_tools(&self) -> Vec<Value> {
        self.all_tools.iter().map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema
                }
            })
        }).collect()
    }
    
    /// 验证工具参数
    pub fn validate_tool_arguments(&self, tool_name: &str, arguments: &Value) -> Result<()> {
        let tool = self.find_tool(tool_name)
            .ok_or_else(|| anyhow::anyhow!("Tool not found: {}", tool_name))?;
        
        // 这里可以添加 JSON Schema 验证
        // 简单实现：检查必需字段
        if let Some(required) = tool.input_schema.get("required") {
            if let Some(required_fields) = required.as_array() {
                for field in required_fields {
                    if let Some(field_name) = field.as_str() {
                        if !arguments.get(field_name).is_some() {
                            return Err(anyhow::anyhow!("Missing required field: {}", field_name));
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
    
    /// 获取工具统计信息
    pub fn get_stats(&self) -> HashMap<String, usize> {
        let mut stats = HashMap::new();
        for (server_name, tools) in &self.server_tools {
            stats.insert(server_name.clone(), tools.len());
        }
        stats
    }
}
