//! Plan 页面主组件：组合左侧计划详情面板与右侧面板。
//!
//! 灵活模式右侧渲染 `FlexiblePage`；
//! 周密模式右侧渲染占位内容（聊天功能开发中）。
//! 计划模式在创建时固定，不可在聊天中切换。

use std::sync::Arc;

use crate::components::alert_dialog::{
    AlertDialog, AlertDialogAction, AlertDialogActions, AlertDialogCancel, AlertDialogDescription,
    AlertDialogTitle,
};
use crate::components::resizable_panel::ResizablePanel;
use dioxus::prelude::*;

use crate::context::StorageContext;
use crate::storage::entities::plan;

use super::flexible::FlexiblePage;
use super::left_panel::PlanLeftPanel;
use super::sessions_panel::SessionPanel;
use super::shared::session::use_provide_session_manager;

/// 本页面专属样式（按需加载）。
const PLAN_CSS: Asset = asset!("/assets/plan.css");
/// ResizablePanel 所需样式（按需加载）。
const RESIZABLE_CSS: Asset = asset!("/assets/resizable_panel.css");

#[component]
pub fn PlanPage(plan_id: String, on_back: EventHandler<()>) -> Element {
    // ── 全局 Context：storage 由启动门保证就绪 ──
    let storage: Arc<StorageContext> = use_context();

    // ── 会话状态管理中心：plan 级共享单例，注入到 context，供 flexible / 会话抽屉等订阅当前会话切换 ──
    use_provide_session_manager();

    // ── 清除消息确认弹窗 ──
    let mut show_clear_dialog = use_signal_sync(|| false);

    // ── 加载计划元数据（use_resource 异步资源；随 plan_id 变化自动重载并取消旧任务） ──
    let pid = plan_id.clone();
    let plan_repo_for_load = storage.plan_repo();
    let plan_resource = use_resource(move || {
        let pid = pid.clone();
        let plan_repo = plan_repo_for_load.clone();
        async move {
            // 进入 plan 先确保存在当前会话（不存在则新建默认「未命名会话」并写回 current_session_id），
            // 这样随后取到的 plan 数据即带有 current_session_id，供后续会话/版本逻辑使用。
            plan_repo.init_session(&pid).await?;
            plan_repo.find_by_id(&pid).await
        }
    });

    // ── 直接从 resource 读取（官网风格）：三态 —— Pending / Err / Ready ──
    // 渲染期读 value 即订阅，资源完成时本组件重渲染，无需再往 signal 中转。
    let plan_loading = plan_resource.value().read().is_none();
    let plan_error: Option<String> = match &*plan_resource.value().read_unchecked() {
        Some(Err(e)) => Some(e.to_string()),
        _ => None,
    };
    let plan_model: Option<plan::Model> = match &*plan_resource.value().read_unchecked() {
        Some(Ok(Some(model))) => Some(model.clone()),
        _ => None,
    };

    // ── 获取 plan_repo 用于删除 ──
    let plan_repo = storage.plan_repo();

    // ── 清除消息记录回调：删除 chat_messages ──
    let on_confirm_clear = {
        let pid = plan_id.clone();
        let chat_msg_repo = storage.chat_message_repo();
        move |_: ()| {
            let pid = pid.clone();
            let repo = chat_msg_repo.clone();
            spawn(async move {
                if let Err(e) = repo.delete_by_plan_id(&pid).await {
                    tracing::error!("清除消息失败: {}", e);
                }
            });
        }
    };

    // ── 删除计划回调：删除关联消息 + 计划记录，随后返回列表页 ──
    let on_delete_plan = {
        let pid = plan_id.clone();
        let plan_repo = plan_repo.clone();
        let chat_msg_repo = storage.chat_message_repo();
        let on_back = on_back;
        move |_: ()| {
            let pid = pid.clone();
            let plan_repo = plan_repo.clone();
            let chat_msg_repo = chat_msg_repo.clone();
            let on_back = on_back;
            spawn(async move {
                if let Err(e) = chat_msg_repo.delete_by_plan_id(&pid).await {
                    tracing::error!("删除消息失败: {}", e);
                }
                if let Err(e) = plan_repo.delete(&pid).await {
                    tracing::error!("删除计划失败: {}", e);
                }
                on_back.call(());
            });
        }
    };

    // ── 会话面板初值：当前会话 id（来自 plan.current_session_id） ──
    let panel_session_id = plan_model
        .as_ref()
        .and_then(|m| m.current_session_id.clone())
        .unwrap_or_default();

    rsx! {
        document::Stylesheet { href: PLAN_CSS }
        document::Stylesheet { href: RESIZABLE_CSS }
        // 三态互斥渲染：加载中 / 加载失败 / 就绪
        if plan_loading {
            div { class: "plan-loading",
                div { class: "plan-loading__spinner" }
                div { class: "plan-loading__text", "正在进入计划…" }
            }
        } else if let Some(err) = plan_error.as_ref() {
            div { class: "plan-error",
                div { class: "plan-error__title", "加载计划失败" }
                div { class: "plan-error__detail", "{err}" }
            }
        } else {
        div { class: "plan-page",
            ResizablePanel {
                left: rsx! {
                    PlanLeftPanel {
                        plan_id: plan_id.clone(),
                        on_back: on_back,
                        plan_info: plan_model.clone(),
                        on_delete: on_delete_plan,
                    }
                },
                center: rsx! {
                    SessionPanel {
                        plan_id: plan_id.clone(),
                        session_id: panel_session_id.clone(),
                    }
                },
                right: {
                    if plan_model.as_ref().map(|m| m.mode.as_str()) == Some("flexible") {
                        rsx! { FlexiblePage { plan_id: plan_id.clone(), session_id: panel_session_id.clone() } }
                    } else {
                        render_chat_panel_placeholder()
                    }
                },
            }
        }
        }

        // ── 清除消息确认弹窗 ──
        AlertDialog {
            open: show_clear_dialog(),
            on_open_change: move |v: bool| show_clear_dialog.set(v),
            AlertDialogTitle { "清除消息记录？" }
            AlertDialogDescription {
                "确定要清空所有对话消息吗？此操作不可撤销，计划本身会被保留。"
            }
            AlertDialogActions {
                AlertDialogCancel { "取消" }
                AlertDialogAction {
                    on_click: {
                        let mut on_confirm_clear = on_confirm_clear.clone();
                        move |_| {
                            show_clear_dialog.set(false);
                            on_confirm_clear(());
                        }
                    },
                    "清除"
                }
            }
        }
    }
}

/// 渲染右侧聊天面板占位（周密模式）：聊天功能开发中。
fn render_chat_panel_placeholder() -> Element {
    rsx! {
        div { class: "chat-panel",
            div {
                style: "display: flex; align-items: center; justify-content: center; flex: 1; color: var(--text-secondary, #999); font-size: 14px;",
                "聊天功能开发中…"
            }
        }
    }
}
