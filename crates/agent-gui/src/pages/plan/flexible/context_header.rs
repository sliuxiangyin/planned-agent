//! ContextHeader 组件：历史上下文折叠区。
//!
//! 仅当 `context_snapshot` 有数据（version > 0）时渲染，
//! 默认折叠为一行概要，点击展开后显示四 tab 切换。

use dioxus::prelude::*;

use super::super::types::PlanFlexibleSnapshot;

/// ContextHeader 组件属性。
#[derive(Props, Clone, PartialEq)]
pub struct ContextHeaderProps {
    /// 从 plans_flexible 加载的四字段快照（None = 首次执行，不渲染）
    pub snapshot: Option<PlanFlexibleSnapshot>,
}

/// 四 tab 标签。
#[derive(Clone, Copy, PartialEq)]
enum ContextTab {
    Todos,
    Summary,
    Params,
    OutputSchema,
}

impl ContextTab {
    fn label(&self) -> &'static str {
        match self {
            ContextTab::Todos => "TODO步骤",
            ContextTab::Summary => "执行轨迹",
            ContextTab::Params => "参数定义",
            ContextTab::OutputSchema => "输出格式",
        }
    }
}

/// 渲染四字段中当前选中 tab 的内容。
fn render_tab_content(snapshot: &PlanFlexibleSnapshot, tab: ContextTab) -> Element {
    match tab {
        ContextTab::Todos => {
            let text = if snapshot.todos.is_empty() {
                "（无记录）".to_string()
            } else {
                format_todos_text(&snapshot.todos)
            };
            rsx! {
                div { class: "context-header__tab-content",
                    pre { class: "context-header__text", "{text}" }
                }
            }
        }
        ContextTab::Summary => {
            let text = if snapshot.previous_summary.is_empty() {
                "（无记录）".to_string()
            } else {
                snapshot.previous_summary.clone()
            };
            rsx! {
                div { class: "context-header__tab-content",
                    div { class: "context-header__text context-header__text--summary",
                        "{text}"
                    }
                }
            }
        }
        ContextTab::Params => {
            let text = if snapshot.params.is_empty() || snapshot.params == "[]" {
                "（未固化参数）".to_string()
            } else {
                format_params_text(&snapshot.params)
            };
            rsx! {
                div { class: "context-header__tab-content",
                    pre { class: "context-header__text", "{text}" }
                }
            }
        }
        ContextTab::OutputSchema => {
            let text = if snapshot.output_schema.is_empty() {
                "（未记录）".to_string()
            } else {
                snapshot.output_schema.clone()
            };
            rsx! {
                div { class: "context-header__tab-content",
                    pre { class: "context-header__text", "{text}" }
                }
            }
        }
    }
}

#[component]
pub fn ContextHeader(props: ContextHeaderProps) -> Element {
    let snapshot = match &props.snapshot {
        Some(s) if s.has_data() => s.clone(),
        _ => return rsx! {},
    };

    let mut expanded = use_signal(|| false);
    let mut active_tab = use_signal(|| ContextTab::Todos);

    let version_label = format!("v{}", snapshot.version);

    rsx! {
        div {
            class: if expanded() {
                "context-header context-header--expanded"
            } else {
                "context-header"
            },
            // 折叠/展开行
            div {
                class: "context-header__bar",
                onclick: move |_| expanded.toggle(),
                span { class: "context-header__icon", "📋" }
                span { class: "context-header__label", "历史上下文 ({version_label})" }
                span { class: "context-header__toggle",
                    if expanded() { "收起 ▲" } else { "展开 ▼" }
                }
            }
            // 展开内容：tab 行 + 内容
            if expanded() {
                div { class: "context-header__tabs",
                    for tab in &[ContextTab::Todos, ContextTab::Summary, ContextTab::Params, ContextTab::OutputSchema] {
                        {
                            let t = *tab;
                            let is_active = active_tab() == t;
                            rsx! {
                                button {
                                    class: if is_active {
                                        "context-header__tab context-header__tab--active"
                                    } else {
                                        "context-header__tab"
                                    },
                                    onclick: move |_| active_tab.set(t),
                                    "{t.label()}"
                                }
                            }
                        }
                    }
                }
                div { class: "context-header__divider" }
                {render_tab_content(&snapshot, active_tab())}
            }
        }
    }
}

// ── 格式化辅助 ──

fn format_todos_text(todos_json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(todos_json) {
        Ok(json) => {
            if let Some(steps) = json.get("steps").and_then(|s| s.as_array()) {
                steps
                    .iter()
                    .enumerate()
                    .map(|(i, step)| {
                        let intent = step
                            .get("intent")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(无描述)");
                        let expected = step
                            .get("expected_output")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if expected.is_empty() {
                            format!("{}. {}\n", i + 1, intent)
                        } else {
                            format!("{}. {}\n   → {}\n", i + 1, intent, expected)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("")
            } else {
                todos_json.to_string()
            }
        }
        Err(_) => todos_json.to_string(),
    }
}

fn format_params_text(params_json: &str) -> String {
    match serde_json::from_str::<Vec<serde_json::Value>>(params_json) {
        Ok(params) => params
            .iter()
            .map(|p| {
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let desc = p
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let example = p
                    .get("example")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if example.is_empty() {
                    format!("- {}: {}\n", name, desc)
                } else {
                    format!("- {}: {}（上次: {}）\n", name, desc, example)
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        Err(_) => params_json.to_string(),
    }
}
