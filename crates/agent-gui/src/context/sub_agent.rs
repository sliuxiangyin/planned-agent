//! SubAgent 快速注册工具
//!
//! 封装「构造 Tool + SubAgentRunner + 注册」的重复模式，
//! 减少页面组件中的样板代码。

use std::sync::Arc;

use serde_json::Value;

use planned_agent::chat::{ChatConfig, SubAgentResultCallback, SubAgentRunner};
use planned_agent_core::mcp::types::Tool;

use super::{AiContext, PromptContext, ToolsContext};

/// 从三个 Context 快速创建并注册一个 SubAgent。
///
/// 封装了 Tool 构造、SubAgentRunner 构造、ToolRegistry 注册三个步骤。
///
/// # 参数
/// - `tool_name` — 工具名（如 `"flexible_step1"`）
/// - `description` — 工具描述
/// - `input_schema` — JSON Schema
/// - `config` — 子 agent 的 `ChatConfig`（含 `system_prompt_template` 等）
/// - `depth` — 当前嵌套深度（通常为 1）
/// - `max_depth` — 最大允许嵌套深度（通常为 2）
pub fn register_sub_agent(
    ai_ctx: &AiContext,
    tools_ctx: &ToolsContext,
    prompt_ctx: &PromptContext,
    tool_name: &str,
    description: &str,
    input_schema: Value,
    config: ChatConfig,
    depth: u32,
    max_depth: u32,
    result_callback: Option<Arc<dyn SubAgentResultCallback>>,
) {
    let tool = Tool {
        name: tool_name.to_string(),
        description: description.to_string(),
        input_schema,
    };

    let runner = SubAgentRunner::new(
        (*ai_ctx.manager).clone(),
        tools_ctx.registry.clone(),
        prompt_ctx.manager.clone(),
        config,
        depth,
        max_depth,
        result_callback,
    );

    tools_ctx
        .registry
        .register_sub_agent(tool, Arc::new(runner));

    tracing::info!("已注册子 Agent: {} ", tool_name);
}
