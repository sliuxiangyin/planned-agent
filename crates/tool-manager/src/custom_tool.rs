use async_trait::async_trait;
use anyhow::Result;
use serde_json::Value;
use planned_agent_core::mcp::types::ToolResult;
use planned_agent_core::tool_registry::{ToolExecutor, ToolCategory};

/// 自定义工具执行器
/// 
/// 用于包装用户自定义的工具执行逻辑
pub struct CustomToolExecutor {
    tool_name: String,
    categories: Vec<ToolCategory>,
    execute_fn: Box<dyn Fn(Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send>> + Send + Sync>,
}

impl CustomToolExecutor {
    /// 创建新的自定义工具执行器（使用函数闭包）
    pub fn new<F, Fut>(
        tool_name: String,
        categories: Vec<ToolCategory>,
        execute_fn: F,
    ) -> Self 
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<ToolResult>> + Send + 'static,
    {
        let execute_fn = Box::new(move |args: Value| -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolResult>> + Send>> {
            Box::pin(execute_fn(args))
        });
        Self { tool_name, categories, execute_fn }
    }
    
    /// 获取工具名称
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }
    
    /// 获取工具分类
    pub fn categories(&self) -> &[ToolCategory] {
        &self.categories
    }
}

#[async_trait]
impl ToolExecutor for CustomToolExecutor {
    async fn execute(&self, _tool_name: &str, arguments: Value) -> Result<ToolResult> {
        (self.execute_fn)(arguments).await
    }
    
    fn name(&self) -> &str {
        &self.tool_name
    }
    
    fn description(&self) -> &str {
        "Custom tool executor"
    }
    
    fn supported_tools(&self) -> Vec<String> {
        vec![self.tool_name.clone()]
    }
}
