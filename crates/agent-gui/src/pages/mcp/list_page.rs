//! MCP 服务列表视图 —— 嵌套于 `SettingsPage` 右侧内容区
//!
//! 注意：组件名沿用 `McpListPage` 仅为与历史的命名连续，
//! 实际**不**挂载到 `AppRouter` 作为顶级 page，不自带顶栏 / 返回箭头；
//! 由 `SettingsPage` 在其右侧内容区嵌渲染，顶栏与"返回"由父级 layout 提供。

use dioxus::prelude::*;
use planned_agent_mcp_rmcp::McpConfigManager;

use crate::context::{McpContext, ToolsContext};

#[component]
pub fn McpListPage(
    on_edit: EventHandler<String>,
    on_add: EventHandler<()>,
) -> Element {
    let config_mgr = McpConfigManager::new(McpConfigManager::DEFAULT_PATH);

    let mut config = use_signal(|| config_mgr.load_config().unwrap_or_default());

    // 正在刷新的 server name
    let mut refreshing = use_signal(|| None::<String>);

    // 获取 contexts
    let mcp_resource = use_context::<Resource<Option<std::sync::Arc<McpContext>>>>();
    let tools_resource = use_context::<Resource<Option<std::sync::Arc<ToolsContext>>>>();

    let mcp_ctx = mcp_resource.read().as_ref().and_then(|x| x.clone());
    let tools_ctx = tools_resource.read().as_ref().and_then(|x| x.clone());

    rsx! {
        div { class: "settings-mcp-manager",
            div { class: "settings-mcp-manager__header",
                h3 { class: "settings-mcp-manager__title", "服务器列表" }
                button {
                    class: "settings-mcp-add-btn",
                    onclick: move |_| on_add.call(()),
                    "+ 添加服务器"
                }
            }

            div { class: "settings-mcp-list",
                for server in config.read().servers.iter() {
                    {
                        let server_name = server.name.clone();
                        let is_refreshing = refreshing.read().as_ref() == Some(&server_name);
                        let command_str = format!(
                            "{} {}",
                            server.server_command,
                            server.server_args.join(" ")
                        );
                        let has_cache = !server.cached_tools.is_empty();
                        let cache_count = server.cached_tools.len();
                        let cats = server.categories.clone().unwrap_or_default();

                        rsx! {
                            div {
                                class: format!(
                                    "settings-mcp-card {}",
                                    if is_refreshing { "settings-mcp-card--refreshing" } else { "" }
                                ),
                                div { class: "settings-mcp-card__body",
                                    div { class: "settings-mcp-card__header",
                                        span { class: "settings-mcp-card__name", "{server.name}" }
                                        span {
                                            class: "settings-mcp-card__status",
                                            title: if has_cache { "已缓存工具" } else { "无缓存，点击刷新获取" },
                                            if has_cache { "● {cache_count} 个工具" } else { "○ 无缓存" }
                                        }
                                    }
                                    div { class: "settings-mcp-card__command", "{command_str}" }
                                    if !cats.is_empty() {
                                        div { class: "settings-mcp-card__categories",
                                            for cat in cats.iter() {
                                                span { class: "settings-mcp-tag", "{cat}" }
                                            }
                                        }
                                    }
                                }
                                div { class: "settings-mcp-card__actions",
                                    // 刷新工具按钮（保留原 McpContext::refresh_tools 调用）
                                    button {
                                        class: format!(
                                            "settings-mcp-action-btn settings-mcp-action-btn--refresh {}",
                                            if is_refreshing { "settings-mcp-action-btn--loading" } else { "" }
                                        ),
                                        disabled: is_refreshing,
                                        onclick: {
                                            let server_name = server_name.clone();
                                            let mcp = mcp_ctx.clone();
                                            let tools = tools_ctx.clone();
                                            move |_| {
                                                let server_name = server_name.clone();
                                                let mcp = mcp.clone();
                                                let tools = tools.clone();
                                                refreshing.set(Some(server_name.clone()));
                                                spawn(async move {
                                                    if let (Some(m), Some(t)) = (mcp, tools) {
                                                        match m.refresh_tools(&server_name, &t.registry).await {
                                                            Ok((_, count)) => {
                                                                tracing::info!(
                                                                    "刷新完成: {} → {} tools",
                                                                    server_name, count
                                                                );
                                                            }
                                                            Err(e) => {
                                                                tracing::warn!(
                                                                    "刷新失败: {} → {}",
                                                                    server_name, e
                                                                );
                                                            }
                                                        }
                                                    }
                                                    refreshing.set(None);
                                                });
                                            }
                                        },
                                        if is_refreshing { "刷新中..." } else { "🔄 刷新工具" }
                                    }
                                    // 编辑按钮：切换到 Editor 视图（settings 内嵌路由）
                                    button {
                                        class: "settings-mcp-action-btn",
                                        onclick: {
                                            let server_name = server_name.clone();
                                            move |_| on_edit.call(server_name.clone())
                                        },
                                        "编辑"
                                    }
                                    // 删除按钮（保留原 cfg_mgr.delete_server）
                                    button {
                                        class: "settings-mcp-action-btn settings-mcp-action-btn--danger",
                                        onclick: {
                                            let server_name = server_name.clone();
                                            let cfg_mgr = config_mgr.clone();
                                            move |_| {
                                                if let Ok(c) = cfg_mgr.delete_server(&server_name) {
                                                    config.set(c);
                                                }
                                            }
                                        },
                                        "删除"
                                    }
                                }
                            }
                        }
                    }
                }

                if config.read().servers.is_empty() {
                    div { class: "settings-mcp-list__empty",
                        "暂无 MCP 服务器配置，点击右上角添加"
                    }
                }
            }
        }
    }
}
