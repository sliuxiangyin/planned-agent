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

use super::left_panel::PlanLeftPanel;
use super::flexible::FlexiblePage;
use super::sessions_panel::SessionPanel;
use super::shared::load_plan_data::load_plan_data as load_plan_data_shared;
use super::shared::session::use_provide_session_manager;
use super::states::PlanState;
use super::types::{ParamDef, PlanInfo};

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

    // ── 计划信息（从 DB 异步加载） ──
    let plan_info = use_signal_sync(|| None::<PlanInfo>);

    // ── 计划模式（从 DB 加载后固定） ──
    let plan_mode = use_signal_sync(|| None::<String>);

    // ── 计划版本号（成功保存后递增，通知 PlanTodoView 重新加载） ──
    let plan_version = use_signal_sync(|| 0u32);

    // ── 已固化的参数定义（清晰度检查勾选后暂存，确认生成时随事件落库） ──
    let plan_params = use_signal_sync(Vec::<ParamDef>::new);

    // ── 清除消息确认弹窗 ──
    let mut show_clear_dialog = use_signal_sync(|| false);

    let mut plan = PlanState {
        plan_info,
        plan_mode,
        plan_version,
        plan_params,
    };

    // ── 加载计划元数据 ──
    let pid = plan_id.clone();
    let plan_repo_for_load = storage.plan_repo();
    use_effect(move || {
        let pid = pid.clone();
        let plan_repo = plan_repo_for_load.clone();
        spawn(async move {
            if let Some(info) = load_plan_data_shared(pid, plan_repo).await {
                plan.plan_info.set(Some(info.clone()));
                plan.set_mode(info.mode);
            }
        });
    });

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

    rsx! {
        document::Stylesheet { href: PLAN_CSS }
        document::Stylesheet { href: RESIZABLE_CSS }
        div { class: "plan-page",
            ResizablePanel {
                left: rsx! {
                    PlanLeftPanel {
                        plan_id: plan_id.clone(),
                        on_back: on_back,
                        plan_info: plan.plan_info,
                        on_delete: on_delete_plan,
                    }
                },
                center: rsx! {
                    SessionPanel {
                        plan_id: plan_id.clone(),
                        on_select: move |session_id| {
                            // 会话切换与 ChatService 重建在「会话监听」环节实现，
                            // 此处先记录，由会话监听/多会话接线后续消费。
                            tracing::info!("会话列表选中: {session_id}");
                        },
                        on_create: move |_| {
                            // 「新建会话」目前仅搭 UI 上抛；创建新会话/版本由后续接线实现。
                            tracing::info!("新建会话点击（行为未接线）");
                        },
                    }
                },
                right: {
                    let mode = plan.mode();
                    if mode == "flexible" {
                        rsx! { FlexiblePage { plan_id: plan_id.clone() } }
                    } else {
                        render_chat_panel_placeholder()
                    }
                },
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

