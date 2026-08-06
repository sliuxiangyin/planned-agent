//! 设置页面主组件（layout 模式）
//!
//! 布局：顶栏 → 左侧导航 Tab → 右侧详情区域
//!
//! 右侧详情区域根据当前 tab 渲染对应"视图"。
//! MCP 服务以前承担"入口卡片 + 列表 + 表单"三层；现改为：
//! - `SettingsTab::McpService` 直接嵌套渲染 `McpListPage`
//! - `McpListPage` 内部点击"添加"或"编辑"时，通过内部状态 `McpView` 切换到 `McpEditorPage`
//! - 左侧 nav 与顶栏复用，符合 nested layout 模式

use dioxus::prelude::*;

use crate::components::page_header::PageHeader;
use crate::context::ToolsContext;
use crate::pages::mcp::{McpEditorPage, McpListPage};
use super::components::tool_list::ToolList;
use super::types::SettingsTab;

/// 本页面样式（按需加载）。
const SETTINGS_CSS: Asset = asset!("/assets/settings.css");

/// MCP 服务在 SettingsPage 内的视图状态（列表 vs 编辑）
#[derive(Clone, PartialEq)]
enum McpView {
    /// 列表视图
    List,
    /// 编辑视图（None = 添加，Some(name) = 编辑原 server）
    Edit(Option<String>),
}

#[component]
pub fn SettingsPage(
    on_back: EventHandler<()>,
) -> Element {
    let mut active_tab = use_signal(|| SettingsTab::ToolManagement);
    // 进入 SettingsPage 时，初始为 List 视图
    let mut mcp_view = use_signal(|| McpView::List);

    // 获取 ToolsContext
    let tools_resource = use_context::<Resource<Option<std::sync::Arc<ToolsContext>>>>();
    let tools_ctx = tools_resource.read().as_ref().and_then(|x| x.clone());

    rsx! {
        document::Stylesheet { href: SETTINGS_CSS }
        div { class: "settings-page",
            // ═══════════════════════════════════════════════════════
            // 顶栏（PageHeader 组件）
            // ═══════════════════════════════════════════════════════
            PageHeader {
                title: "设置".to_string(),
                on_back: Some(on_back),
                back_label: Some("← 返回指挥中心".to_string()),
            }

            // ═══════════════════════════════════════════════════════
            // 主体：左侧导航 + 右侧内容
            // ═══════════════════════════════════════════════════════
            div { class: "settings-body",
                // ── 左侧导航 ──
                nav { class: "settings-nav",
                    for tab in SettingsTab::all() {
                        {
                            let t = tab.clone();
                            let is_active = *active_tab.read() == t;
                            let is_enabled = t.enabled();

                            rsx! {
                                button {
                                    class: format!(
                                        "settings-nav__item {} {}",
                                        if is_active { "settings-nav__item--active" } else { "" },
                                        if !is_enabled { "settings-nav__item--disabled" } else { "" }
                                    ),
                                    disabled: !is_enabled,
                                    title: if !is_enabled { "即将上线" } else { "" },
                                    onclick: move |_| {
                                        if !is_enabled {
                                            return;
                                        }
                                        active_tab.set(t.clone());
                                        // 切到 McpService tab 时重置内部视图为列表，
                                        // 避免上一次的"编辑中间态"残留。
                                        if matches!(t, SettingsTab::McpService) {
                                            mcp_view.set(McpView::List);
                                        }
                                    },
                                    span { class: "settings-nav__icon", "{t.icon()}" }
                                    span { class: "settings-nav__label", "{t.label()}" }
                                    if !is_enabled {
                                        span { class: "settings-nav__badge", "即将上线" }
                                    }
                                }
                            }
                        }
                    }
                }

                // ── 右侧详情 ──
                div { class: "settings-content",
                    match active_tab.read().clone() {
                        SettingsTab::General => rsx! {
                            div { class: "settings-placeholder",
                                span { class: "settings-placeholder__icon", "⚙" }
                                p { "通用设置 - 即将上线" }
                            }
                        },
                        SettingsTab::Model => rsx! {
                            div { class: "settings-placeholder",
                                span { class: "settings-placeholder__icon", "🤖" }
                                p { "模型设置 - 即将上线" }
                            }
                        },
                        SettingsTab::ToolManagement => rsx! {
                            div { class: "settings-tool-management",
                                ToolList {
                                    tools_ctx: tools_ctx.clone(),
                                }
                            }
                        },
                        // MCP 服务：右侧直接嵌套渲染列表/编辑视图（不再有"入口卡片"中间层）
                        SettingsTab::McpService => rsx! {
                            div { class: "settings-tool-management",
                                match mcp_view.read().clone() {
                                    McpView::List => rsx! {
                                        McpListPage {
                                            on_add:  move |_| mcp_view.set(McpView::Edit(None)),
                                            on_edit: move |name: String| mcp_view.set(McpView::Edit(Some(name))),
                                        }
                                    },
                                    McpView::Edit(editing_name) => rsx! {
                                        McpEditorPage {
                                            editing_name: editing_name.clone(),
                                            on_back:  move |_| mcp_view.set(McpView::List),
                                            on_saved: move |_| mcp_view.set(McpView::List),
                                        }
                                    },
                                }
                            }
                        },
                    }
                }
            }
        }
    }
}
