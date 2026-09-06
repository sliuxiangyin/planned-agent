//! 工具定义构建与 UI 工具分发。

mod ui;

pub(super) use ui::parse_ui_actions;

use std::collections::HashSet;

use planned_agent_core::ai::types::{FunctionDefinition, ToolDefinition, ToolType};
use planned_agent_core::mcp::types::Tool;
use planned_agent_core::tool_registry::ToolCategory;

use crate::chat::state::State;

pub(super) fn build_tool_definitions<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
>(
    state: &State<PM>,
) -> Vec<ToolDefinition> {
    let allowed_tools = state.config.lock().unwrap().allowed_tools.clone();
    // (tool, categories)：enabled 工具在 registry 中按名唯一。
    let enabled = state.tool_registry.get_enabled_tools_with_categories();

    let selected: Vec<Tool> = match &allowed_tools {
        // None：全部工具（含 Utility / SubAgent，不过滤）——保持向后兼容。
        None => enabled.into_iter().map(|(t, _)| t).collect(),
        Some(tokens) => select_tools_by_tokens(enabled, tokens),
    };

    selected
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

/// 按 `allowed_tools` 的 token 列表挑选工具（各 token 取并集）。
///
/// 支持三种 token：
/// - `"all"`：加载**除 `Utility` 与 `SubAgent` 两类外**的全部工具。
/// - 分类名（如 `"Utility"` / `"SubAgent"` / `"Browser"` …）：加载该分类下全部工具。
/// - 精确工具名（如 `"flexible_state"`）：加载该工具（仅当确实存在且 enabled）。
///
/// 未知分类名或不存在工具会被静默忽略。
fn select_tools_by_tokens(
    enabled: Vec<(Tool, Vec<ToolCategory>)>,
    tokens: &[String],
) -> Vec<Tool> {
    let mut wanted: HashSet<String> = HashSet::new();

    for token in tokens {
        match token.as_str() {
            "all" => {
                // 排除任一分类为 Utility / SubAgent 的工具
                for (t, cats) in &enabled {
                    let excluded = cats
                        .iter()
                        .any(|c| matches!(c, ToolCategory::Utility | ToolCategory::SubAgent));
                    if !excluded {
                        wanted.insert(t.name.clone());
                    }
                }
            }
            _ => {
                if let Some(cat) = ToolCategory::from_name(token) {
                    // 分类名：加载该分类下全部 enabled 工具
                    for (t, cats) in &enabled {
                        if cats.contains(&cat) {
                            wanted.insert(t.name.clone());
                        }
                    }
                } else if enabled.iter().any(|(t, _)| t.name == *token) {
                    // 精确工具名（确认存在才放行）
                    wanted.insert(token.clone());
                }
                // 其它：既不是已知分类也不存在该工具 → 忽略
            }
        }
    }

    enabled
        .into_iter()
        .filter(|(t, _)| wanted.contains(&t.name))
        .map(|(t, _)| t)
        .collect()
}

pub(super) const UI_TOOL_NAMES: &[&str] = &["request_user_action"];

#[cfg(test)]
mod tests {
    use super::*;
    use planned_agent_core::tool_registry::ToolCategory;

    /// 构造一个带分类的 (Tool, categories) 供 select_tools_by_tokens 测试。
    fn tool(name: &str, cats: &[ToolCategory]) -> (Tool, Vec<ToolCategory>) {
        (
            Tool {
                name: name.to_string(),
                description: String::new(),
                input_schema: serde_json::json!({}),
            },
            cats.to_vec(),
        )
    }

    fn names(out: Vec<Tool>) -> Vec<String> {
        out.into_iter().map(|t| t.name).collect()
    }

    #[test]
    fn all_excludes_utility_and_subagent() {
        let enabled = vec![
            tool("browser", &[ToolCategory::Browser]),
            tool("data", &[ToolCategory::Data]),
            tool("flexible_state", &[ToolCategory::Utility]),
            tool("step1", &[ToolCategory::SubAgent]),
            // 多分类：只要含 Utility / SubAgent 之一即被剔除
            tool("hybrid", &[ToolCategory::Browser, ToolCategory::Utility]),
        ];
        let out = select_tools_by_tokens(enabled.clone(), &["all".to_string()]);
        assert_eq!(names(out), vec!["browser", "data"]);
    }

    #[test]
    fn all_plus_named_utility_is_readded() {
        let enabled = vec![
            tool("browser", &[ToolCategory::Browser]),
            tool("flexible_state", &[ToolCategory::Utility]),
        ];
        let tokens = vec!["all".to_string(), "flexible_state".to_string()];
        let out = select_tools_by_tokens(enabled.clone(), &tokens);
        // 顺序随 enabled 保序：browser 先、flexible_state 补回在后
        assert_eq!(names(out), vec!["browser", "flexible_state"]);
    }

    #[test]
    fn category_token_loads_whole_category() {
        let enabled = vec![
            tool("a", &[ToolCategory::Browser]),
            tool("b", &[ToolCategory::Browser]),
            tool("c", &[ToolCategory::File]),
        ];
        let out = select_tools_by_tokens(enabled.clone(), &["Browser".to_string()]);
        assert_eq!(names(out), vec!["a", "b"]);
    }

    #[test]
    fn none_named_or_unknown_tokens_yield_nothing() {
        // 无 "all"：空 token 与未知 token 都不放行任何工具
        let enabled = vec![tool("a", &[ToolCategory::Browser])];
        let out = select_tools_by_tokens(
            enabled.clone(),
            &["NotACategory".to_string(), "missing_tool".to_string()],
        );
        assert!(out.is_empty());

        // 空 token 列表同样不放行
        let out2 = select_tools_by_tokens(enabled, &[]);
        assert!(out2.is_empty());
    }

    #[test]
    fn named_nonexistent_tool_is_ignored() {
        let enabled = vec![tool("real", &[ToolCategory::File])];
        let tokens = vec!["all".to_string(), "ghost_tool".to_string()];
        let out = select_tools_by_tokens(enabled.clone(), &tokens);
        // 点名的不存在工具被忽略，不影响 all 结果
        assert_eq!(names(out), vec!["real"]);
    }
}
