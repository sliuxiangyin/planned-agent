//! 工具定义构建与 UI 工具分发。

mod ui;

pub(super) use ui::parse_ui_actions;

use planned_agent_core::ai::types::{FunctionDefinition, ToolDefinition, ToolType};

use crate::chat::state::State;

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

pub(super) const UI_TOOL_NAMES: &[&str] = &["request_user_action"];
