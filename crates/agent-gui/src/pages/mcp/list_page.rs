//! MCP 服务列表视图 —— 嵌套于 `SettingsPage` 右侧内容区
//!
//! 注意：组件名沿用 `McpListPage` 仅为与历史的命名连续，
//! 实际**不**挂载到 `AppRouter` 作为顶级 page，不自带顶栏 / 返回箭头；
//! 由 `SettingsPage` 在其右侧内容区嵌渲染，顶栏与"返回"由父级 layout 提供。
//!
//! ## 数据来源（统一视图）
//!
//! 通过 [`McpContext::load_servers`] 拿到 `Vec<McpServerView>`——这是 bundle
//! 已 join 好的 config + status 视图，**不**需要在 UI 侧手动按 name 配对。
//!
//! ## 变更监听
//!
//! 任何对 MCP 数据的写入后（编辑器保存、刷新完成、删除等），调用方应 `bump()`
//! [`McpChangeNotifier`]。本组件通过 `use_effect` 订阅 version signal，
//! 每次 bump 都会重新加载视图。

use dioxus::prelude::*;
use dioxus_primitives::alert_dialog::{
    AlertDialogRoot, AlertDialogContent, AlertDialogTitle,
    AlertDialogDescription, AlertDialogActions, AlertDialogAction, AlertDialogCancel,
};
use planned_agent_core::types::ConnectionError;
use planned_agent_mcp_rmcp::storage::{LastStatus, ServerStatus};

use crate::context::{McpChangeNotifier, McpContext, ToolsContext};

/// 单个 MCP server 卡片的运行时状态
#[derive(Debug, Clone)]
enum CardStatus {
    /// 就绪：已缓存/已刷新 N 个工具（绿色）
    Ready(u32),
    /// 正在刷新/连接中（灰色）
    Connecting,
    /// 刷新失败（红色，可点击弹错误详情）
    Failed(ConnectionError),
}

/// 错误类别标签
fn error_kind_label(err: &ConnectionError) -> &'static str {
    match err {
        ConnectionError::Timeout { .. } => "超时",
        ConnectionError::Spawn { .. } => "启动失败",
        ConnectionError::Handshake { .. } => "握手失败",
    }
}

/// 从持久化的 [`ServerStatus`] 还原成 UI 的 [`CardStatus`]
///
/// 由于 [`ServerStatus`] 不保留完整 `ConnectionError`（只保留 kind + 截断消息），
/// 这里对于 Failed 状态重建一个最小化的 `ConnectionError::Handshake` 仅供 dialog 展示。
fn to_card_status(s: &ServerStatus) -> Option<CardStatus> {
    match s.status {
        LastStatus::Ready { tool_count } => Some(CardStatus::Ready(tool_count)),
        LastStatus::Connecting => Some(CardStatus::Connecting),
        LastStatus::Failed => {
            let reason = s.error_message.clone().unwrap_or_else(|| {
                format!(
                    "（上次连接失败：kind={}）",
                    s.error_kind.as_deref().unwrap_or("unknown")
                )
            });
            Some(CardStatus::Failed(ConnectionError::Handshake {
                reason,
                stderr_tail: None,
            }))
        }
    }
}

/// 从视图的 status 字段直接还原 CardStatus（已 join 好的数据）
fn view_to_card_status(view: &planned_agent_mcp_rmcp::McpServerView) -> Option<CardStatus> {
    view.status.as_ref().and_then(to_card_status)
}

#[component]
pub fn McpListPage(
    on_edit: EventHandler<String>,
    on_add: EventHandler<()>,
) -> Element {
    // ── 获取 contexts（先取 ctx，以便 use_signal 闭包捕获） ──
    let mcp_resource = use_context::<Resource<Option<std::sync::Arc<McpContext>>>>();
    let tools_resource = use_context::<Resource<Option<std::sync::Arc<ToolsContext>>>>();

    let mcp_ctx = mcp_resource.read().as_ref().and_then(|x| x.clone());
    let tools_ctx = tools_resource.read().as_ref().and_then(|x| x.clone());

    // ── 统一视图信号（bundle.load_servers() 一次性拿到 config + status 已 join 的视图） ──
    // 冷启动即从 KV / 文件中加载历史状态，无需手动按 name 配对
    let mut views = use_signal({
        let mcp_ctx = mcp_ctx.clone();
        move || mcp_ctx.as_ref().map(|c| c.load_servers()).unwrap_or_default()
    });

    // ── 变更监听：McpChangeNotifier 触发时重新加载视图 ──
    // 让编辑器保存/刷新完成后自动反映到列表（不需要手动 reload）
    let notifier = use_context::<McpChangeNotifier>();
    {
        let mcp_ctx = mcp_ctx.clone();
        let mut views = views.clone();
        use_effect(move || {
            // 订阅 version 变化（每次 bump 触发这里）
            let _ = notifier.version();
            if let Some(c) = mcp_ctx.as_ref() {
                views.set(c.load_servers());
            }
        });
    }

    // AlertDialog 的开/关
    let mut dialog_open = use_signal(|| false);
    let mut dialog_server = use_signal(|| String::new());

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
                for view in views.read().iter() {
                    {
                        let server_name = view.name().to_string();
                        let command_str = view.command_str();
                        let has_cache = view.has_cached_tools();
                        let cache_count = view.cached_tools_count();
                        let cats = view.config.categories.clone().unwrap_or_default();
                        let card_status = view_to_card_status(view);
                        let is_connecting = matches!(card_status, Some(CardStatus::Connecting));

                        rsx! {
                            div {
                                class: format!(
                                    "settings-mcp-card {}",
                                    if is_connecting { "settings-mcp-card--refreshing" } else { "" }
                                ),
                                div { class: "settings-mcp-card__body",
                                    div { class: "settings-mcp-card__header",
                                        span { class: "settings-mcp-card__name", "{view.config.name}" }
                                        {
                                            match card_status {
                                                Some(CardStatus::Ready(count)) => {
                                                    rsx! {
                                                        span {
                                                            class: "settings-mcp-card__status settings-mcp-card__status--ok",
                                                            "● {count} 个工具"
                                                        }
                                                    }
                                                }
                                                Some(CardStatus::Connecting) => {
                                                    rsx! {
                                                        span {
                                                            class: "settings-mcp-card__status settings-mcp-card__status--loading",
                                                            "● 连接中..."
                                                        }
                                                    }
                                                }
                                                Some(CardStatus::Failed(_)) => {
                                                    let srv = server_name.clone();
                                                    rsx! {
                                                        span {
                                                            class: "settings-mcp-card__status settings-mcp-card__status--error",
                                                            onclick: move |_| {
                                                                dialog_server.set(srv.clone());
                                                                dialog_open.set(true);
                                                            },
                                                            title: "点击查看失败详情",
                                                            "● 连接失败"
                                                        }
                                                    }
                                                }
                                                None => {
                                                    rsx! {
                                                        span {
                                                            class: "settings-mcp-card__status",
                                                            title: if has_cache { "已缓存工具" } else { "无缓存，点击刷新获取" },
                                                            if has_cache { "● {cache_count} 个工具" } else { "○ 无缓存" }
                                                        }
                                                    }
                                                }
                                            }
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
                                    button {
                                        class: format!(
                                            "settings-mcp-action-btn settings-mcp-action-btn--refresh {}",
                                            if is_connecting { "settings-mcp-action-btn--loading" } else { "" }
                                        ),
                                        disabled: is_connecting,
                                        onclick: {
                                            let server_name = server_name.clone();
                                            let mcp = mcp_ctx.clone();
                                            let tools = tools_ctx.clone();
                                            let notifier = notifier;
                                            move |_| {
                                                let srv = server_name.clone();
                                                let m = mcp.clone();
                                                let t = tools.clone();
                                                let notif = notifier;

                                                // 即时 UI 反馈：先标记 Connecting
                                                // （bundle 内部 fetch_and_cache_tools 完成后会写 status，
                                                //  之后 reload 视图会自然更新）
                                                let mut cur = views.read().clone();
                                                if let Some(v) = cur.iter_mut().find(|v| v.name() == srv.as_str()) {
                                                    let now = ServerStatus::now();
                                                    v.status = Some(ServerStatus {
                                                        status: LastStatus::Connecting,
                                                        error_kind: None,
                                                        error_message: None,
                                                        attempt_at: now,
                                                    });
                                                }
                                                views.set(cur);

                                                spawn(async move {
                                                    if let (Some(mgr), Some(tctx)) = (m, t) {
                                                        match mgr.refresh_tools(&srv, &tctx.registry).await {
                                                                Ok((_, count)) => {
                                                                    tracing::info!("刷新完成: {} → {} tools", srv, count);
                                                                    // bundle 已写 status (Ready(n))，reload 视图
                                                                    views.set(mgr.load_servers());
                                                                }
                                                                Err((e, _conn_err)) => {
                                                                    // bundle 已自动 record_failure，无需 UI 再调
                                                                    tracing::warn!("刷新失败: {} → {}", srv, e);
                                                                    views.set(mgr.load_servers());
                                                                }
                                                            }
                                                        // 通知其他监听者（如 list_page 其他实例、settings 等）
                                                        notif.bump();
                                                    } else {
                                                        // ctx 不可用，清空视图
                                                        views.set(Vec::new());
                                                    }
                                                });
                                            }
                                        },
                                        if is_connecting { "刷新中..." } else { "🔄 刷新工具" }
                                    }
                                    button {
                                        class: "settings-mcp-action-btn",
                                        disabled: is_connecting,  // 刷新中禁用编辑
                                        onclick: {
                                            let server_name = server_name.clone();
                                            move |_| on_edit.call(server_name.clone())
                                        },
                                        "编辑"
                                    }
                                    button {
                                        class: "settings-mcp-action-btn settings-mcp-action-btn--danger",
                                        disabled: mcp_ctx.is_none() || is_connecting,
                                        onclick: {
                                            let server_name = server_name.clone();
                                            let mcp_ctx = mcp_ctx.clone();
                                            let notifier = notifier;
                                            move |_| {
                                                if let Some(c) = mcp_ctx.as_ref() {
                                                    // 走 McpContext::delete_server 包装，自动联动清理 status
                                                    if c.delete_server(&server_name).is_ok() {
                                                        // 通知所有监听者重新加载
                                                        notifier.bump();
                                                        // 立即本地更新（不等 use_effect）
                                                        views.set(c.load_servers());
                                                    }
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

                if views.read().is_empty() {
                    div { class: "settings-mcp-list__empty",
                        "暂无 MCP 服务器配置，点击右上角添加"
                    }
                }
            }
        }

        // ── AlertDialog：连接失败详情弹窗 ──────────────────────────────
        {
            let dname = dialog_server.read().clone();

            // 该 server 当前是否处于"连接中"（用于禁用重试按钮）
            let is_server_connecting = if dname.is_empty() {
                false
            } else {
                views.read().iter()
                    .find(|v| v.name() == dname.as_str())
                    .and_then(|v| v.status.as_ref())
                    .map(|s| matches!(s.status, LastStatus::Connecting))
                    .unwrap_or(false)
            };

            let error_info: Option<(String, ConnectionError)> =
                if dname.is_empty() {
                    None
                } else {
                    views.read().iter()
                        .find(|v| v.name() == dname.as_str())
                        .and_then(|v| v.status.as_ref())
                        .and_then(|s| to_card_status(s))
                        .and_then(|cs| match cs {
                            CardStatus::Failed(err) => Some((dname.clone(), err)),
                            _ => None,
                        })
                };

            if let Some((server_name, ref err)) = error_info {
                let error_msg = err.message();
                let kind = error_kind_label(err);

                rsx! {
                    AlertDialogRoot {
                        open: dialog_open(),
                        on_open_change: move |v: bool| dialog_open.set(v),
                        AlertDialogContent {
                            class: "mcp-error-dialog",
                            AlertDialogTitle { "⚠ MCP 服务连接失败" }
                            AlertDialogDescription {
                                div { class: "mcp-error-dialog__meta",
                                    p { class: "mcp-error-dialog__row",
                                        span { class: "mcp-error-dialog__label", "服务器" }
                                        span { class: "mcp-error-dialog__value", "{server_name}" }
                                    }
                                    p { class: "mcp-error-dialog__row",
                                        span { class: "mcp-error-dialog__label", "类别" }
                                        span { class: "mcp-error-dialog__value", "{kind}" }
                                    }
                                }
                                div { class: "mcp-error-dialog__detail",
                                    pre { "{error_msg}" }
                                }
                            }
                            AlertDialogActions {
                                AlertDialogAction {
                                    aria_disabled: is_server_connecting,  // 刷新中禁用重试（语义+无障碍）
                                    on_click: {
                                        let srv = server_name.clone();
                                        let mcp = mcp_ctx.clone();
                                        let tools = tools_ctx.clone();
                                        let notifier = notifier;
                                        move |_| {
                                            let s = srv.clone();
                                            let m = mcp.clone();
                                            let t = tools.clone();
                                            let notif = notifier;

                                            // 即时 UI：Connecting
                                            let mut cur = views.read().clone();
                                            if let Some(v) = cur.iter_mut().find(|v| v.name() == s.as_str()) {
                                                let now = ServerStatus::now();
                                                v.status = Some(ServerStatus {
                                                    status: LastStatus::Connecting,
                                                    error_kind: None,
                                                    error_message: None,
                                                    attempt_at: now,
                                                });
                                            }
                                            views.set(cur);

                                            spawn(async move {
                                                if let (Some(mgr), Some(tctx)) = (m, t) {
                                                    match mgr.refresh_tools(&s, &tctx.registry).await {
                                                        Ok((_, _count)) => {
                                                            // bundle 已写 status，reload 视图
                                                            views.set(mgr.load_servers());
                                                        }
                                                        Err((_e, _conn_err)) => {
                                                            // bundle 已自动 record_failure，reload 视图
                                                            views.set(mgr.load_servers());
                                                        }
                                                    }
                                                    notif.bump();
                                                }
                                            });
                                        }
                                    },
                                    if is_server_connecting { "重试中..." } else { "重试" }
                                }
                                AlertDialogCancel { "关闭" }
                            }
                        }
                    }
                }
            } else {
                rsx! {}
            }
        }
    }
}