//! Plan 页面主组件：组合左侧计划详情面板与右侧聊天面板。
//!
//! 计划模式在创建时固定，不可在聊天中切换。

use std::sync::Arc;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::markdown::Markdown;
use crate::components::page_header::PageHeader;
use crate::components::resizable_panel::ResizablePanel;
use crate::components::scroll_area::ScrollArea;
use crate::components::textarea::Textarea;
use dioxus::prelude::*;
use planned_agent_core::types::{Message, MessageContent, MessageRole};

use crate::context::{InitStatus, ModuleState, StorageContext};
use crate::services::chat_service::use_chat_service;

use super::components::chat_ui_actions_view::ChatUIActionsView;
use super::components::plan_todo_view::PlanTodoView;
use super::components::reasoning_view::ReasoningView;

use super::chat::{handle_user_action, send_message};
use super::types::{display_text, role_css_class, PendingUIState, PlanGeneratedEvent};

/// 本页面专属样式（按需加载）。
const PLAN_CSS: Asset = asset!("/assets/plan.css");
/// ResizablePanel 所需样式（按需加载）。
const RESIZABLE_CSS: Asset = asset!("/assets/resizable_panel.css");

/// 从 DB 加载的计划信息
#[derive(Clone)]
struct PlanInfo {
    name: String,
    mode: String,
    status: String,
    created_at: String,
}

#[component]
pub fn PlanPage(plan_id: String, on_back: EventHandler<()>) -> Element {
    // ── 全局 Context ──
    let init_status = use_context::<Memo<InitStatus>>();
    let storage = use_context::<Resource<Option<Arc<StorageContext>>>>();

    // ── 计划信息（从 DB 异步加载） ──
    let mut plan_info = use_signal_sync(|| None::<PlanInfo>);

    // ── 聊天状态 ──
    let mut messages = use_signal_sync(|| vec![]);
    let mut input_text = use_signal_sync(String::new);
    let streaming_idx = use_signal_sync(|| None::<usize>);
    let pending_ui = use_signal_sync(|| None::<PendingUIState>);
    let mut reasoning_texts = use_signal_sync(|| Vec::<Option<String>>::new());

    // ── 计划模式（从 DB 加载后固定） ──
    let mut plan_mode = use_signal_sync(|| None::<String>);

    // ── 计划生成事件 ──
    let plan_generated = use_signal_sync(|| None::<PlanGeneratedEvent>);

    // ── 加载计划元数据 + 历史消息 ──
    let pid = plan_id.clone();
    use_effect(move || {
        let storage_opt = storage.read().as_ref().and_then(|x| x.as_ref()).cloned();
        if let Some(ctx) = storage_opt {
            let plan_repo = ctx.plan_repo.clone();
            let msg_repo = ctx.message_repo.clone();
            let pid = pid.clone();
            spawn(async move {
                // 加载计划元数据
                if let Ok(Some(plan_model)) = plan_repo.find_by_id(&pid).await {
                    plan_info.set(Some(PlanInfo {
                        name: plan_model.name,
                        mode: plan_model.mode.clone(),
                        status: plan_model.status,
                        created_at: plan_model.created_at,
                    }));
                    plan_mode.set(Some(plan_model.mode));
                }
                // 加载历史消息
                if let Ok(msg_list) = msg_repo.find_by_plan_id(&pid).await {
                    let loaded: Vec<Message> = msg_list
                        .into_iter()
                        .map(|m| Message {
                            role: match m.role.as_str() {
                                "user" => MessageRole::User,
                                "assistant" => MessageRole::Assistant,
                                "system" => MessageRole::System,
                                "tool" => MessageRole::Tool,
                                _ => MessageRole::User,
                            },
                            content: if m.content.is_empty() {
                                None
                            } else {
                                Some(MessageContent::Text { text: m.content })
                            },
                            ..Default::default()
                        })
                        .collect();
                    messages.set(loaded);
                    // 对齐 reasoning_texts 长度
                    reasoning_texts.set(vec![None; messages.read().len()]);
                }
            });
        }
    });

    // ── 根据 plan_mode 派生 system prompt 模板路径 ──
    let system_prompt_template = use_memo(move || {
        plan_mode.read().as_ref().map(|mode| match mode.as_str() {
            "flexible" => "chat/flexible_system".to_string(),
            "thorough" => "chat/thorough_system".to_string(),
            _ => "chat/thorough_system".to_string(),
        })
    });

    // ── Chat Service ──
    let chat_signal = use_chat_service(system_prompt_template.into());

    // ── 按钮可用性 ──
    let can_create = init_status.read().ai.state == ModuleState::Ready
        && init_status.read().prompt.state == ModuleState::Ready;

    let sidx = *streaming_idx.read();

    // ── 获取 message_repo 用于持久化 ──
    let message_repo = storage
        .read()
        .as_ref()
        .and_then(|x| x.as_ref())
        .map(|ctx| ctx.message_repo.clone());

    // ── 右侧聊天面板 ──
    let chat_panel = rsx! {
        div { class: "chat-panel",
            PlanTodoView { plan_generated }

            // 消息展示区
            div { class: "chat-messages",
                ScrollArea {
                    div { class: "chat-messages__list",
                        for (idx, msg) in messages.read().iter().enumerate() {
                            {
                                let is_streaming = sidx == Some(idx);
                                let text = display_text(msg);
                                let class = format!(
                                    "chat-message chat-message--{} {}",
                                    role_css_class(&msg.role),
                                    if is_streaming { "chat-message--streaming" } else { "" }
                                );

                                let r_text: String = reasoning_texts
                                    .read()
                                    .get(idx)
                                    .and_then(|o| o.clone())
                                    .unwrap_or_default();
                                let has_reasoning = !r_text.is_empty();
                                let show_streaming_cursor =
                                    is_streaming && text.is_empty() && !has_reasoning;

                                rsx! {
                                    div {
                                        class: "{class}",
                                        if has_reasoning {
                                            ReasoningView {
                                                text: r_text,
                                                is_streaming: is_streaming,
                                            }
                                        }
                                        if show_streaming_cursor {
                                            "▍"
                                        } else if !text.is_empty() {
                                            Markdown { text: text.to_string() }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 待处理的 UI 交互
            {
                let ui = pending_ui.read();
                if let Some(ref pending) = *ui {
                    let p = pending.clone();
                    rsx! {
                        ChatUIActionsView {
                            message: p.message.clone(),
                            actions: p.actions.clone(),
                            on_action: move |(action, choice)| {
                                let mode = plan_mode.read().clone().unwrap_or_default();
                                handle_user_action(
                                    action,
                                    choice,
                                    p.clone(),
                                    messages,
                                    reasoning_texts,
                                    streaming_idx,
                                    pending_ui,
                                    chat_signal,
                                    mode,
                                    plan_generated,
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
                    value: "{input_text}",
                    disabled: !can_create,
                    oninput: move |e: FormEvent| input_text.set(e.value()),
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
                                            input_text,
                                            messages,
                                            reasoning_texts,
                                            streaming_idx,
                                            pending_ui,
                                            pid.clone(),
                                            repo.clone(),
                                        );
                                    }
                                }
                            }
                        }
                    },
                }
                // ── 操作行：发送 / 停止 ──
                div { class: "chat-input-area__actions",
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
                                                input_text,
                                                messages,
                                                reasoning_texts,
                                                streaming_idx,
                                                pending_ui,
                                                pid.clone(),
                                                repo.clone(),
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
                                if let Some(ref chat) = *chat_signal.read() {
                                    chat.stop();
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
    };

    // ── 左侧计划详情面板 ──
    let plan_name = plan_info
        .read()
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| format!("计划 {}", plan_id));
    let plan_mode_label = plan_info
        .read()
        .as_ref()
        .map(|p| match p.mode.as_str() {
            "thorough" => "周密模式".to_string(),
            _ => "灵活模式".to_string(),
        })
        .unwrap_or_default();
    let plan_status_label = plan_info
        .read()
        .as_ref()
        .map(|p| match p.status.as_str() {
            "generated" => "已生成".to_string(),
            _ => "待生成".to_string(),
        })
        .unwrap_or_default();

    rsx! {
        document::Stylesheet { href: PLAN_CSS }
        document::Stylesheet { href: RESIZABLE_CSS }
        div { class: "plan-page",
            ResizablePanel {
                initial_left_percent: 70.0,
                min_left_percent: 25.0,
                max_left_percent: 75.0,
                left: rsx! {
                    div { class: "plan-left-panel",
                        // ── Header topbar：返回 + 计划名称（PageHeader 组件） ──
                        PageHeader {
                            title: plan_name.clone(),
                            on_back: Some(on_back),
                            class: Some("page-header--nested".to_string()),
                        }
                        div { class: "plan-left-panel__divider" }
                        // ── 计划详情 ──
                        div { class: "plan-left-panel__details",
                            div { class: "plan-detail-item",
                                span { class: "plan-detail-item__label", "模式" }
                                span { class: "plan-detail-item__value", "{plan_mode_label}" }
                            }
                            div { class: "plan-detail-item",
                                span { class: "plan-detail-item__label", "状态" }
                                span { class: "plan-detail-item__value", "{plan_status_label}" }
                            }
                            if let Some(ref info) = *plan_info.read() {
                                div { class: "plan-detail-item",
                                    span { class: "plan-detail-item__label", "创建时间" }
                                    span { class: "plan-detail-item__value", "{info.created_at}" }
                                }
                            }
                        }
                    }
                },
                right: chat_panel,
            }
        }
    }
}
