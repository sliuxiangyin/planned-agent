//! 工具定义构建与 UI 工具分发。
//!
//! - [`build_tool_definitions`]：从 [`ToolRegistry`] 构建 LLM 侧的
//!   `Vec<ToolDefinition>`，按 [`ChatConfig::allowed_tools`] 白名单过滤；
//! - [`parse_ui_actions`]：`request_user_action` 参数解析（见 `tools/ui.rs`）。

mod ui;

pub(super) use ui::parse_ui_actions;

use planned_agent_core::ai::types::{FunctionDefinition, ToolDefinition, ToolType};

use crate::chat::state::State;

/// 从 ToolRegistry 构建 ToolDefinition 列表，按 `allowed_tools` 白名单过滤。
pub(super) fn build_tool_definitions<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &State<PM>,
) -> Vec<ToolDefinition> {
    let all = state.tool_registry.get_all_tools();
    let allowed_tools = state.config.lock().unwrap().allowed_tools.clone();
    let filtered: Vec<_> = match &allowed_tools {
        None => all,
        Some(whitelist) => all
            .into_iter()
            .filter(|t| whitelist.contains(&t.name))
            .collect(),
    };

    filtered
        .into_iter()
        .map(|t| ToolDefinition {
            r#type: ToolType::Function,
            function: FunctionDefinition {
                name: t.name,
                description: Some(t.description),
                parameters: Some(t.input_schema),
                strict: None,
            },
        })
        .collect()
}

/// 当前唯一被识别的 UI 工具名（决定走 UI 流程而非后端工具执行）。
pub(super) const UI_TOOL_NAMES: &[&str] = &["request_user_action"];
