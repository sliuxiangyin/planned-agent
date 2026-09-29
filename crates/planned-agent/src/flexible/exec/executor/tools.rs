//! 工具表组装。

use planned_agent_core::ai::types::{FunctionDefinition, ToolDefinition, ToolType};
use planned_agent_core::mcp::types::Tool;
use planned_agent_tool_manager::ToolRegistry; 

/// 按工具名精确取定义（输出整理步只需要 `builtin_read_file`）。
pub(super) fn tool_definitions_for_names(tools: &ToolRegistry, names: &[&str]) -> Vec<ToolDefinition> {
    tools
        .get_enabled_tools_with_categories()
        .into_iter()
        .filter(|(tool, _)| names.contains(&tool.name.as_str()))
        .map(|(tool, _)| to_tool_definition(tool))
        .collect()
}

/// `Tool` → LLM 侧的 `ToolDefinition`。
pub(super) fn to_tool_definition(tool: Tool) -> ToolDefinition {
    ToolDefinition {
        r#type: ToolType::Function,
        function: FunctionDefinition {
            name: tool.name,
            description: Some(tool.description),
            parameters: Some(tool.input_schema),
            strict: None,
        },
    }
}
