//! Plan 页面主组件：组合左侧计划详情面板与右侧面板。
//!
//! 灵活模式右侧渲染 FlexibleWorkflow（三段式结构化工作流）；
//! 周密模式右侧渲染原有聊天面板。
//! 计划模式在创建时固定，不可在聊天中切换。

use std::sync::Arc;

use crate::components::alert_dialog::{
    AlertDialog, AlertDialogAction, AlertDialogActions, AlertDialogCancel, AlertDialogDescription,
    AlertDialogTitle,
};
use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::resizable_panel::ResizablePanel;
use crate::components::textarea::Textarea;
use dioxus::prelude::*;

use crate::context::{InitStatus, ModuleState, StorageContext};
use crate::services::chat_service::{use_chat_service, ChatServiceSignal};
use crate::storage::repository::MessageRepo;

use super::chat::{handle_user_action, send_message};
use super::components::chat_ui_actions_view::ChatUIActionsView;
use super::components::plan_todo_view::PlanTodoView;
use super::left_panel::PlanLeftPanel;
use super::flexible::workflow::FlexibleWorkflow;
use super::message::MessageListView;
use super::shared::load_plan_data::load_plan_data as load_plan_data_shared;
use super::states::{ChatState, PlanState, WorkflowState};
use super::types::{
    ParamDef, PendingUIState, PlanGeneratedEvent, PlanInfo, PlanFlexibleSnapshot,
    WorkflowPhase,
};

/// 本页面专属样式（按需加载）。
const PLAN_CSS: Asset = asset!("/assets/plan.css");
/// ResizablePanel 所需样式（按需加载）。
const RESIZABLE_CSS: Asset = asset!("/assets/resizable_panel.css");

#[component]
pub fn PlanPage(plan_id: String, on_back: EventHandler<()>) -> Element {
    // ── 全局 Context ──
    let init_status = use_context::<Memo<InitStatus>>();
    let storage = use_context::<Resource<Option<Arc<StorageContext>>>>();

    // ── 计划信息（从 DB 异步加载） ──
    let plan_info = use_signal_sync(|| None::<PlanInfo>);

    // ── 聊天状态 ──
    let messages = use_signal_sync(|| vec![]);
    let input_text = use_signal_sync(String::new);
    let streaming_idx = use_signal_sync(|| None::<usize>);
    let pending_ui = use_signal_sync(|| None::<PendingUIState>);
    let reasoning_texts = use_signal_sync(|| Vec::<Option<String>>::new());

    // ── 计划模式（从 DB 加载后固定） ──
    let plan_mode = use_signal_sync(|| None::<String>);

    // ── 计划生成事件 ──
    let plan_generated = use_signal_sync(|| None::<PlanGeneratedEvent>);

    // ── 计划版本号（save_flexible_plan 成功后递增，通知 PlanTodoView 重新加载） ──
    let plan_version = use_signal_sync(|| 0u32);

    // ── 已固化的参数定义（清晰度检查勾选后暂存，确认生成时随事件落库） ──
    let plan_params = use_signal_sync(Vec::<ParamDef>::new);

    // ── 自动需求开关（灵活模式：开启后 AI 自动确认模糊需求，无需用户手动点击） ──
    let auto_requirement = use_signal_sync(|| false);

    // ── 灵活模式工作流状态 ──
    let workflow_phase = use_signal_sync(|| WorkflowPhase::Idle);
    let workflow_requirement_text = use_signal_sync(String::new);
    let workflow_execution_steps = use_signal_sync(|| vec![]);
    let workflow_pending_ui = use_signal_sync(|| None::<PendingUIState>);
    let workflow_context_snapshot = use_signal_sync(|| None::<PlanFlexibleSnapshot>);
    let workflow_param_values = use_signal_sync(|| vec![]);
    let workflow_input_params_enabled = use_signal_sync(|| false);
    let workflow_output_params_enabled = use_signal_sync(|| false);
    let workflow_phase_output = use_signal_sync(String::new);

    let mut workflow = WorkflowState {
        phase: workflow_phase,
        requirement_text: workflow_requirement_text,
        execution_steps: workflow_execution_steps,
        pending_ui: workflow_pending_ui,
        context_snapshot: workflow_context_snapshot,
        param_values: workflow_param_values,
        input_params_enabled: workflow_input_params_enabled,
        output_params_enabled: workflow_output_params_enabled,
        phase_output: workflow_phase_output,
    };

    // ── 清除消息确认弹窗 ──
    let mut show_clear_dialog = use_signal_sync(|| false);

    // ── 分组状态结构体：收敛所有信号，后续函数仅传入 struct ──
    let mut chat = ChatState {
        messages,
        reasoning_texts,
        streaming_idx,
        pending_ui,
        input_text,
    };
    let plan = PlanState {
        plan_info,
        plan_mode,
        plan_generated,
        plan_version,
        plan_params,
    };

    // ── 加载计划元数据 + 历史消息 / plans_flexible 快照 ──
    let pid = plan_id.clone();
    use_effect(move || {
        let storage_opt = storage.read().as_ref().and_then(|x| x.as_ref()).cloned();
        if let Some(ctx) = storage_opt {
            spawn(load_plan_data_shared(
                pid.clone(),
                ctx.plan_repo.clone(),
                ctx.message_repo.clone(),
                Some(ctx.plan_flexible_repo.clone()),
                chat,
                plan,
                Some(workflow),
            ));
        }
    });

    // ── 根据 plan_mode 派生 system prompt 模板路径 ──
    let system_prompt_template = use_memo(move || {
        plan.plan_mode
            .read()
            .as_ref()
            .map(|mode| match mode.as_str() {
                "flexible" => "flexible/flexible_system".to_string(),
                "thorough" => "thorough/thorough_system".to_string(),
                _ => "thorough/thorough_system".to_string(),
            })
    });

    // ── Chat Service ──
    let chat_signal = use_chat_service(system_prompt_template.into());

    // ── 按钮可用性 ──
    let can_create = init_status.read().ai.state == ModuleState::Ready
        && init_status.read().prompt.state == ModuleState::Ready;

    // ── 获取 message_repo / plan_repo 用于持久化与删除 ──
    let message_repo = storage
        .read()
        .as_ref()
        .and_then(|x| x.as_ref())
        .map(|ctx| ctx.message_repo.clone());
    let plan_repo = storage
        .read()
        .as_ref()
        .and_then(|x| x.as_ref())
        .map(|ctx| ctx.plan_repo.clone());

    // ── 清除消息记录回调：乐观清空内存信号，异步删除 DB 记录 ──
    let on_confirm_clear = {
        let pid = plan_id.clone();
        let repo = message_repo.clone();
        move |_: ()| {
            chat.clear();
            let pid = pid.clone();
            let repo = repo.clone();
            spawn(async move {
                if let Some(ref repo) = repo {
                    if let Err(e) = repo.delete_by_plan_id(&pid).await {
                        tracing::error!("清除消息失败: {}", e);
                    }
                }
            });
        }
    };

    // ── 删除计划回调：删除关联消息 + 计划记录，随后返回列表页 ──
    let on_delete_plan = {
        let pid = plan_id.clone();
        let plan_repo = plan_repo.clone();
        let msg_repo = message_repo.clone();
        let on_back = on_back;
        move |_: ()| {
            let pid = pid.clone();
            let plan_repo = plan_repo.clone();
            let msg_repo = msg_repo.clone();
            let on_back = on_back;
            spawn(async move {
                if let Some(repo) = msg_repo {
                    if let Err(e) = repo.delete_by_plan_id(&pid).await {
                        tracing::error!("删除消息失败: {}", e);
                    }
                }
                if let Some(repo) = plan_repo {
                    if let Err(e) = repo.delete(&pid).await {
                        tracing::error!("删除计划失败: {}", e);
                    }
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
                initial_left_percent: 70.0,
                min_left_percent: 25.0,
                max_left_percent: 75.0,
                left: rsx! {
                    PlanLeftPanel {
                        plan_id: plan_id.clone(),
                        on_back: on_back,
                        plan_info: plan.plan_info,
                        on_delete: on_delete_plan,
                    }
                },
                right: {
                    let mode = plan.mode();
                    if mode == "flexible" {
                        rsx! {
                            FlexibleWorkflow {
                                plan_id: plan_id.clone(),
                                chat_signal: chat_signal,
                                plan: plan,
                                workflow: workflow,
                                storage: storage.clone(),
                            }
                        }
                    } else {
                        render_chat_panel(
                            plan_id.clone(),
                            storage.clone(),
                            chat,
                            plan,
                            chat_signal,
                            can_create,
                            message_repo.clone(),
                            show_clear_dialog,
                            auto_requirement,
                        )
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

/// 渲染右侧聊天面板（周密模式）：PlanTodo + MessageList + 待处理 UI + 输入发送区。
fn render_chat_panel(
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    mut chat: ChatState,
    plan: PlanState,
    chat_signal: ChatServiceSignal,
    can_create: bool,
    message_repo: Option<Arc<MessageRepo>>,
    mut show_clear_dialog: Signal<bool, SyncStorage>,
    mut auto_requirement: Signal<bool, SyncStorage>,
) -> Element {
    let sidx = chat.sidx();
    rsx! {
        div { class: "chat-panel",
            PlanTodoView {
                plan_id: plan_id.clone(),
                storage: storage.clone(),
                plan_generated: plan.plan_generated,
                plan_version: plan.plan_version,
            }

            MessageListView {
                messages: chat.messages,
                reasoning_texts: chat.reasoning_texts,
                streaming_idx: chat.streaming_idx,
            }

            // 待处理的 UI 交互
            {
                let ui = chat.pending_ui.read();
                if let Some(ref pending) = *ui {
                    let p = pending.clone();
                    let pid = plan_id.clone();
                    let msg_repo = message_repo.clone();
                    rsx! {
                        ChatUIActionsView {
                            message: p.message.clone(),
                            actions: p.actions.clone(),
                            on_action: move |(action, choice)| {
                                handle_user_action(
                                    action,
                                    choice,
                                    p.clone(),
                                    chat,
                                    chat_signal,
                                    plan,
                                    pid.clone(),
                                    msg_repo.clone(),
                                );
                            },
                        }
                    }
                } else {
                    rsx! {}
                }
            }

            // 输入发送区
            div { class: "chat-input-area",
                Textarea {
                    placeholder: if can_create { "输入消息..." } else { "等待 AI 与 Prompt 初始化..." },
                    value: "{chat.input_text}",
                    disabled: !can_create,
                    oninput: move |e: FormEvent| chat.input_text.set(e.value()),
                    onkeydown: {
                        let pid = plan_id.clone();
                        let repo = message_repo.clone();
                        move |e: KeyboardEvent| {
                            if e.data.key() == keyboard_types::Key::Enter && !e.data.modifiers().shift() {
                                e.prevent_default();
                                if can_create {
                                    if let Some(ref repo) = repo {
                                        send_message(
                                            chat_signal,
                                            chat,
                                            pid.clone(),
                                            repo.clone(),
                                            plan.mode(),
                                            auto_requirement(),
                                        );
                                    }
                                }
                            }
                        }
                    },
                }
                // ── 操作行：清除消息 / 发送 / 停止 ──
                div { class: "chat-input-area__actions",
                      Button {
                                class: "chat-input-area__icon-btn chat-input-area__icon-btn--clear",
                                variant: ButtonVariant::Ghost,
                                size: ButtonSize::Xs,
                                disabled: sidx.is_some(),
                                title: Some("清除消息记录"),
                                onclick: move |_: MouseEvent| show_clear_dialog.set(true),
                                svg {
                                    xmlns: "http://www.w3.org/2000/svg",
                                    width: "16",
                                    height: "16",
                                    view_box: "0 0 24 24",
                                    fill: "none",
                                    stroke: "currentColor",
                                    stroke_width: "2",
                                    stroke_linecap: "round",
                                    stroke_linejoin: "round",
                                    path { d: "M3 6h18" }
                                    path { d: "M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6" }
                                    path { d: "M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2" }
                                    line { x1: "10", y1: "11", x2: "10", y2: "17" }
                                    line { x1: "14", y1: "11", x2: "14", y2: "17" }
                                }
                            }
                    
                      
                    // 自动需求开关（仅灵活模式显示）
                    if plan.mode() == "flexible" {
                        div { class: "chat-input-area__auto-toggle",
                            span { class: "chat-input-area__auto-toggle-label", "自动需求" }
                            crate::components::switch::Switch {
                                checked: auto_requirement(),
                                on_checked_change: move |v: bool| auto_requirement.set(v),
                            }
                        }
                    }

                    div { class: "chat-input-area__placeholder" }
                    if sidx.is_none() {
                        Button {
                            class: "chat-input-area__icon-btn chat-input-area__icon-btn--send",
                            variant: ButtonVariant::Primary,
                            size: ButtonSize::Xs,
                            disabled: !can_create,
                            title: if !can_create { Some("AI 与 Prompt 初始化完成后才能发送") } else { Some("发送") },
                            onclick: {
                                let pid = plan_id.clone();
                                let repo = message_repo.clone();
                                move |_: MouseEvent| {
                                    if can_create {
                                        if let Some(ref repo) = repo {
                                            send_message(
                                                chat_signal,
                                                chat,
                                                pid.clone(),
                                                repo.clone(),
                                                plan.mode(),
                                                auto_requirement(),
                                            );
                                        }
                                    }
                                }
                            },
                            svg {
                                xmlns: "http://www.w3.org/2000/svg",
                                width: "16",
                                height: "16",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "2",
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                path { d: "M22 2 11 13" }
                                path { d: "m22 2-7 20-4-9-9-4 20-7z" }
                            }
                        }
                    } else {
                        Button {
                            class: "chat-input-area__icon-btn chat-input-area__icon-btn--stop",
                            variant: ButtonVariant::Destructive,
                            size: ButtonSize::Xs,
                            title: Some("停止生成"),
                            onclick: move |_: MouseEvent| {
                                if let Some(ref svc) = *chat_signal.read() {
                                    svc.stop();
                                }
                            },
                            svg {
                                xmlns: "http://www.w3.org/2000/svg",
                                width: "16",
                                height: "16",
                                view_box: "0 0 24 24",
                                fill: "currentColor",
                                rect { x: "6", y: "6", width: "12", height: "12", rx: "1.5" }
                            }
                        }
                    }
                }
            }
        }
    }
}
