//! 工具列表组件：筛选栏 + 工具卡片列表 + 统计栏

use dioxus::prelude::*;
use planned_agent_core::tool_registry::ToolSource;

use crate::context::ToolsContext;
use super::super::types::{CategoryFilter, ToolSourceFilter};

/// 工具列表组件
#[component]
pub fn ToolList(
    tools_ctx: Option<std::sync::Arc<ToolsContext>>,
) -> Element {
    let mut source_filter = use_signal(|| ToolSourceFilter::All);
    let mut category_filter = use_signal(|| CategoryFilter::All);
    let mut search_query = use_signal(String::new);

    // 克隆出来供多个闭包使用
    let tools_ctx_for_memo = tools_ctx.clone();
    let tools_ctx_for_stats = tools_ctx.clone();

    // 统计信息
    let stats = use_signal(move || {
        match &tools_ctx_for_stats {
            Some(c) => c.registry.get_stats(),
            None => planned_agent_tool_manager::types::ToolRegistryStats {
                total: 0, enabled: 0, disabled: 0,
                mcp_count: 0, custom_count: 0, builtin_count: 0,
            },
        }
    });

    // 获取筛选后的工具列表
    let display_tools = use_memo(move || {
        let ctx = match &tools_ctx_for_memo {
            Some(c) => c,
            None => return Vec::new(),
        };

        let all_tools = ctx.registry.get_all_tools();

        all_tools
            .into_iter()
            .filter(|tool| {
                let meta = ctx.registry.get_metadata(&tool.name);

                // 来源筛选
                let source_match = match &*source_filter.read() {
                    ToolSourceFilter::All => true,
                    ToolSourceFilter::Mcp => meta.as_ref().map_or(false, |m| matches!(m.source, ToolSource::Mcp { .. })),
                    ToolSourceFilter::Builtin => meta.as_ref().map_or(false, |m| matches!(m.source, ToolSource::Builtin)),
                    ToolSourceFilter::Custom => meta.as_ref().map_or(false, |m| matches!(m.source, ToolSource::Custom { .. })),
                };
                if !source_match { return false; }

                // 分类筛选
                let category_match = match &*category_filter.read() {
                    CategoryFilter::All => true,
                    CategoryFilter::Specific(cat) => {
                        meta.as_ref().map_or(false, |m| m.categories.contains(cat))
                    }
                };
                if !category_match { return false; }

                // 搜索匹配
                let query = search_query.read();
                if !query.is_empty() {
                    let q = query.to_lowercase();
                    tool.name.to_lowercase().contains(&q)
                        || tool.description.to_lowercase().contains(&q)
                } else {
                    true
                }
            })
            .map(|tool| {
                let meta = ctx.registry.get_metadata(&tool.name);
                let source_label = match &meta.as_ref().map(|m| &m.source) {
                    Some(ToolSource::Mcp { server_name }) => format!("MCP: {}", server_name),
                    Some(ToolSource::Builtin) => "内置".into(),
                    Some(ToolSource::Custom { .. }) => "自定义".into(),
                    _ => "未知".into(),
                };
                let source_class = match &meta.as_ref().map(|m| &m.source) {
                    Some(ToolSource::Mcp { .. }) => "settings-tool-source--mcp",
                    Some(ToolSource::Builtin) => "settings-tool-source--builtin",
                    Some(ToolSource::Custom { .. }) => "settings-tool-source--custom",
                    _ => "",
                };
                ToolDisplayData {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    source_label,
                    source_class: source_class.to_string(),
                    categories: meta.as_ref()
                        .map(|m| m.categories.iter().map(|c| c.description().to_string()).collect())
                        .unwrap_or_default(),
                    enabled: meta.as_ref().map(|m| m.enabled).unwrap_or(false),
                    is_locked: meta.as_ref().map_or(false, |m| {
                        matches!(m.source, ToolSource::Builtin | ToolSource::Custom { .. })
                    }),
                }
            })
            .collect::<Vec<_>>()
    });

    let tools_ctx_for_cards = tools_ctx.clone();

    rsx! {
        div { class: "settings-tool-list",
            // ── 筛选栏 ──
            div { class: "settings-tool-list__filters",
                select {
                    class: "settings-filter-select",
                    onchange: move |e: FormEvent| {
                        let val = e.value();
                        if val == "__all__" {
                            category_filter.set(CategoryFilter::All);
                        } else {
                            for opt in CategoryFilter::all_options() {
                                if opt.label() == val {
                                    category_filter.set(opt);
                                    break;
                                }
                            }
                        }
                    },
                    for opt in CategoryFilter::all_options() {
                        {
                            let val = if matches!(&opt, CategoryFilter::All) {
                                "__all__".to_string()
                            } else {
                                opt.label()
                            };
                            let label = opt.label();
                            let selected = *category_filter.read() == opt;
                            rsx! {
                                option {
                                    value: "{val}",
                                    selected: selected,
                                    "{label}"
                                }
                            }
                        }
                    }
                }

                div { class: "settings-filter-chips",
                    for filter in ToolSourceFilter::all() {
                        button {
                            class: format!(
                                "settings-filter-chip {}",
                                if *source_filter.read() == filter { "settings-filter-chip--active" } else { "" }
                            ),
                            onclick: move |_| source_filter.set(filter.clone()),
                            "{filter.label()}"
                        }
                    }
                }

                input {
                    class: "settings-filter-search",
                    r#type: "text",
                    placeholder: "搜索工具...",
                    value: "{search_query}",
                    oninput: move |e: FormEvent| search_query.set(e.value()),
                }
            }

            // ── 工具列表 ──
            div { class: "settings-tool-list__items",
                if display_tools.read().is_empty() {
                    div { class: "settings-tool-list__empty", "没有匹配的工具" }
                } else {
                    for item in display_tools.read().iter() {
                        {
                            let item = item.clone();
                            let ctx = tools_ctx_for_cards.clone();
                            rsx! {
                                div { class: "settings-tool-card",
                                    div { class: "settings-tool-card__header",
                                        span { class: "settings-tool-card__name", "{item.name}" }
                                        span { class: "settings-tool-source {item.source_class}", "{item.source_label}" }
                                        if item.is_locked {
                                            span {
                                                class: "settings-tool-toggle settings-tool-toggle--locked",
                                                title: if item.source_label == "内置" {
                                                    "内置工具不可修改"
                                                } else {
                                                    "自定义工具暂不支持修改"
                                                },
                                                if item.enabled { "✓" } else { "✗" }
                                            }
                                        } else {
                                            {
                                                let name = item.name.clone();
                                                let enabled = item.enabled;
                                                rsx! {
                                                    button {
                                                        class: format!(
                                                            "settings-tool-toggle {}",
                                                            if enabled { "settings-tool-toggle--on" } else { "settings-tool-toggle--off" }
                                                        ),
                                                        title: if enabled { "点击禁用" } else { "点击启用" },
                                                        onclick: move |_| {
                                                            if let Some(ref c) = ctx {
                                                                let _ = c.registry.set_tool_enabled(&name, !enabled);
                                                            }
                                                        },
                                                        if enabled { "✓ 已启用" } else { "✗ 已禁用" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    p { class: "settings-tool-card__desc", "{item.description}" }
                                    if !item.categories.is_empty() {
                                        div { class: "settings-tool-card__tags",
                                            for cat in &item.categories {
                                                span { class: "settings-tool-tag", "{cat}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── 统计栏 ──
            div { class: "settings-tool-list__stats",
                span { class: "settings-stats-item",
                    "共 ", span { class: "settings-stats-value", "{stats.read().total}" }, " 个工具"
                }
                span { class: "settings-stats-sep", "|" }
                span { class: "settings-stats-item",
                    "启用 ", span { class: "settings-stats-value", "{stats.read().enabled}" }
                }
                span { class: "settings-stats-sep", "|" }
                span { class: "settings-stats-item",
                    "MCP: ", span { class: "settings-stats-value", "{stats.read().mcp_count}" }
                }
                span { class: "settings-stats-sep", "|" }
                span { class: "settings-stats-item",
                    "内置: ", span { class: "settings-stats-value", "{stats.read().builtin_count}" }
                }
                span { class: "settings-stats-sep", "|" }
                span { class: "settings-stats-item",
                    "自定义: ", span { class: "settings-stats-value", "{stats.read().custom_count}" }
                }
            }
        }
    }
}

/// 工具展示数据（扁平化，避免 trait bound 问题）
#[derive(Clone, PartialEq)]
struct ToolDisplayData {
    name: String,
    description: String,
    source_label: String,
    source_class: String,
    categories: Vec<String>,
    enabled: bool,
    is_locked: bool,
}
