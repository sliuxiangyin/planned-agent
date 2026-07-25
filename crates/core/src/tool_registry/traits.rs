use std::sync::Arc;
use serde_json::Value;
use anyhow::Result;
use crate::types::Tool;
use crate::types::ToolResult;
use super::types::ToolCategory;

/// 工具执行器 trait（抽象层）
/// 
/// 定义在 core 中，由 tool-manager 实现
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// 执行工具调用
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult>;
    
    /// 获取执行器名称
    fn name(&self) -> &str;
    
    /// 获取执行器描述
    fn description(&self) -> &str {
        ""
    }
    
    /// 获取此执行器支持的工具名称列表
    fn supported_tools(&self) -> Vec<String>;
    
    /// 检查是否支持指定工具
    fn supports_tool(&self, tool_name: &str) -> bool {
        self.supported_tools().contains(&tool_name.to_string())
    }
}

/// 内置工具提供者 trait（抽象层）
/// 
/// 定义在 core 中，由 tool-manager 实现
pub trait BuiltinToolProvider: Send + Sync {
    /// 获取提供的工具列表
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)>;
    
    /// 获取执行器（Arc 版本，便于共享）
    fn executor(&self) -> Arc<dyn ToolExecutor>;
}
