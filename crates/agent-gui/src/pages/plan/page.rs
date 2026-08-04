//! Plan 页面主组件：组合左侧占位面板与右侧聊天面板。
//!
//! 仅持有 UI 状态与 rsx 树；chat 流程委托给同级的 `chat` 模块。

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::markdown::Markdown;
use crate::components::resizable_panel::ResizablePanel;
use crate::components::scroll_area::ScrollArea;
use crate::components::textarea::Textarea;
use crate::components::todo::{Todo, TodoItemData, TodoStatus};
use dioxus::prelude::*;

use crate::context::{InitStatus, ModuleState};
use crate::services::chat_service::use_chat_service;
use planned_agent::ChatConfig;

use super::components::chat_ui_actions_view::ChatUIActionsView;
use super::components::reasoning_view::ReasoningView;

use super::chat::{handle_user_action, send_message};
use super::types::{display_text, role_css_class, PendingUIState};

/// 本页面专属样式（按需加载）。
const PLAN_CSS: Asset = asset!("/assets/plan.css");
/// ResizablePanel 所需样式（按需加载）。
const RESIZABLE_CSS: Asset = asset!("/assets/resizable_panel.css");

#[component]
pub fn PlanPage(
    plan_id: Option<String>,
    on_back: EventHandler<()>,
) -> Element {
    // ── 全局 Context（main.rs 注入） ──
    let init_status = use_context::<Memo<InitStatus>>();

    // ── 聊天状态（SyncStorage：Send + Sync，可在 spawn 异步任务中持有） ──
    let messages = use_signal_sync(|| {
        vec![]
    });
    let mut input_text = use_signal_sync(String::new);
    // 当前正在流式输出的消息下标（None = 没有消息在流式输出）
    let streaming_idx = use_signal_sync(|| None::<usize>);
    // 待处理的 UI 交互（Agent 请求用户确认/选择/输入）
    let pending_ui = use_signal_sync(|| None::<PendingUIState>);

    // ── 深度思考侧 state ──
    // 与 messages 一一对应；None 表示该消息没有 reasoning（user/tool）
    let reasoning_texts = use_signal_sync(|| Vec::<Option<String>>::new());

    // ── Chat Service 缓存（委托给 services::chat_service） ──
    // 显式传入 ChatConfig：system_prompt_template 指向 prompts/chat/system.toml
    // （当前值与 ChatConfig::default() 相同，但走显式路径便于将来按页面切模板）。
    let chat_signal = use_chat_service(ChatConfig {
        system_prompt_template: Some("chat/system".to_string()),
        enable_thinking:true,
        ..Default::default()
    });

    // ── 按钮可用性 ──
    let can_create = init_status.read().ai.state == ModuleState::Ready
        && init_status.read().prompt.state == ModuleState::Ready;

    // 快照当前流式下标（响应式：streaming_idx 变化时本 rsx 整体重渲）
    let sidx = *streaming_idx.read();

    // ── 右侧聊天面板 ──
    let chat_panel = rsx! {
        div { class: "chat-panel",
            // Todo 计划区，固定 200px（当前使用 mock 数据，后续接 AI 计划）
            div {
                style : "padding:0px",
                Todo {
                items: vec![
                    TodoItemData::new(
                        TodoStatus::Completed,
                        "分析当前项目结构与依赖关系",
                        "读取 Cargo.toml 与 crate 目录结构，识别已实现的核心模块（core、planned-agent、agent-gui 等），确认本次改动边界。",
                    ),
                    TodoItemData::new(
                        TodoStatus::Running,
                        "创建 Todo UI 组件",
                        "在 crates/agent-gui/src/components/todo/ 下新增 mod.rs、component.rs、style.css，复用 dioxus_primitives::accordion 行为，自定义 trigger / content 样式。",
                    ),
                    TodoItemData::new(
                        TodoStatus::Queued,
                        "在 Plan 页面接入 Todo 组件",
                        "在 pages/plan.rs 的右侧聊天面板顶部插入 Todo，固定 200px 高度，列表区支持展开收起，底部执行按钮暂不接业务。",
                    ),
                    TodoItemData::new(
                        TodoStatus::Pending,
                        "后续接入 AI 计划数据流",
                        "从 Assistant 回复中解析计划条目，同步填充 Todo；执行计划时按状态机更新 TodoStatus；本轮仅完成 UI，不实现数据流。",
                    ),
                ],
                on_execute: move |_| {
                    // TODO(后续): 触发 Agent 执行计划
                },
            }
            }

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

                                // reasoning_texts 与 messages 一一对位：
                                // user/tool 行 → None（unwrap 后是空串）
                                let r_text: String = reasoning_texts
                                    .read()
                                    .get(idx)
                                    .and_then(|o| o.clone())
                                    .unwrap_or_default();
                                let has_reasoning = !r_text.is_empty();
                                // 流式光标仅在还没有可显示的 reasoning 时出现
                                let show_streaming_cursor =
                                    is_streaming && text.is_empty() && !has_reasoning;

                                rsx! {
                                    div {
                                        class: "{class}",
                                        // ── 深度思考折叠面板（无 reasoning 时不渲染）──
                                        if has_reasoning {
                                            ReasoningView {
                                                text: r_text,
                                                is_streaming: is_streaming,
                                            }
                                        }
                                        // ── 正文 ──
                                        if show_streaming_cursor {
                                            "▍"
                                        } else if !text.is_empty() {
                                            // Markdown 渲染：pulldown-cmark + ammonia sanitize
                                            Markdown { text: text.to_string() }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 🆕 待处理的 UI 交互（Agent 请求用户操作的按钮/选项）
            {
                let ui = pending_ui.read();
                if let Some(ref pending) = *ui {
                    let p = pending.clone();
                    rsx! {
                        ChatUIActionsView {
                            message: p.message.clone(),
                            actions: p.actions.clone(),
                            on_action: move |(action, choice)| {
                                handle_user_action(
                                    action,
                                    choice,
                                    p.clone(),
                                    messages,
                                    reasoning_texts,
                                    streaming_idx,
                                    pending_ui,
                                    chat_signal,
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
                    placeholder: if can_create { "创建计划..." } else { "等待 AI 与 Prompt 初始化..." },
                    value: "{input_text}",
                    disabled: !can_create,
                    oninput: move |e: FormEvent| input_text.set(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.data.key() == keyboard_types::Key::Enter && !e.data.modifiers().shift() {
                            e.prevent_default();
                            if can_create {
                                send_message(
                                    chat_signal,
                                    input_text,
                                    messages,
                                    reasoning_texts,
                                    streaming_idx,
                                    pending_ui,
                                );
                            }
                        }
                    },
                }
                // ── 操作行：左侧占位（后期会放置其它操作） | 右侧图标按钮（发送 / 停止）──
                div { class: "chat-input-area__actions",
                    // 左侧占位（暂时为空，后续增加其它操作）
                    div { class: "chat-input-area__placeholder" }

                    // 发送图标：未在流式输出时显示
                    if sidx.is_none() {
                        Button {
                            class: "chat-input-area__icon-btn chat-input-area__icon-btn--send",
                            variant: ButtonVariant::Primary,
                            size: ButtonSize::Xs,
                            disabled: !can_create,
                            title: if !can_create { Some("AI 与 Prompt 初始化完成后才能创建") } else { Some("发送") },
                            onclick: move |_: MouseEvent| {
                                if can_create {
                                    send_message(
                                        chat_signal,
                                        input_text,
                                        messages,
                                        reasoning_texts,
                                        streaming_idx,
                                        pending_ui,
                                    );
                                }
                            },
                            // 发送图标（纸飞机，stroke=currentColor 跟随按钮前景色）
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
                        // 停止图标（实心方块，fill=currentColor 跟随按钮前景色）
                        Button {
                            class: "chat-input-area__icon-btn chat-input-area__icon-btn--stop",
                            variant: ButtonVariant::Destructive,
                            size: ButtonSize::Xs,
                            title: Some("停止生成（待接入 stop API）"),
                            onclick: move |_: MouseEvent| {
                                // TODO: 接入 chat_service 的停止逻辑
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
                        // ── 返回按钮 + 计划标题 ──
                        div { class: "plan-left-panel__header",
                            button {
                                class: "plan-back-btn",
                                onclick: move |_| on_back.call(()),
                                title: "返回指挥中心",
                                "← 返回"
                            }
                            div { class: "plan-left-panel__title",
                                if let Some(ref id) = plan_id {
                                    span { class: "plan-left-panel__plan-id", "计划 #{id}" }
                                } else {
                                    span { class: "plan-left-panel__plan-id", "新建计划" }
                                }
                            }
                        }
                        div { class: "plan-left-panel__divider" }
                        span { class: "plan-left-panel__label",
                            "计划详情（待开发）"
                        }
                    }
                },
                right: chat_panel,
            }
        }
    }
}
