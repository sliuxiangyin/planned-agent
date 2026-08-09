//! Chat 测试页主组件：顶栏 + 消息列表（内嵌 request_user_action 卡片）+ 输入区 + 提示词选择器。
//!
//! 纯内存状态；system prompt 模板通过左侧选择器动态热切换
//! （更新信号 → `use_chat_service` 的 effect 自动重建 `ChatService`）。

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::prompt::PromptManager;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::markdown::Markdown;
use crate::components::scroll_area::ScrollArea;
use crate::components::textarea::Textarea;
use crate::context::PromptContext;
use crate::pages::plan::components::chat_ui_actions_view::ChatUIActionsView;
use crate::pages::plan::components::reasoning_view::ReasoningView;
use crate::services::chat_service::{use_chat_service, ChatServiceSignal};

use super::chat_flow::{handle_user_action, send_message, ChatSignals, PendingUI};

/// 本页面自定义样式。
const CHAT_CSS: Asset = asset!("/assets/chat.css");
/// 复用 plan 页的消息列表 / 输入区样式类。
const PLAN_CSS: Asset = asset!("/assets/plan.css");

/// 默认选中的测试模板。
const DEFAULT_TEMPLATE: &str = "chat/request_user_action_demo";

#[component]
pub fn ChatPage(on_back: EventHandler<()>) -> Element {
    // ── 纯内存聊天状态 ──
    let messages = use_signal_sync(|| Vec::<Message>::new());
    let reasoning_texts = use_signal_sync(|| Vec::<Option<String>>::new());
    let streaming_idx = use_signal_sync(|| None::<usize>);
    let pending_ui = use_signal_sync(|| None::<PendingUI>);
    let input_text = use_signal_sync(String::new);
    let mut chat = ChatSignals {
        messages,
        reasoning_texts,
        streaming_idx,
        pending_ui,
        input_text,
    };

    // ── system prompt 模板（响应式，选择器热切换） ──
    let mut template = use_signal_sync(|| Some(DEFAULT_TEMPLATE.to_string()));
    let system_prompt_template = use_memo(move || template.read().clone());
    let chat_signal: ChatServiceSignal = use_chat_service(system_prompt_template.into());

    // ── 可用模板列表（`chat/` 前缀，从 PromptManager 动态加载） ──
    let prompt_resource = use_context::<Resource<Option<Arc<PromptContext>>>>();
    let mut templates = use_signal_sync(|| Vec::<String>::new());
    use_effect(move || {
        let prompt = prompt_resource
            .read()
            .as_ref()
            .and_then(|x| x.as_ref())
            .cloned();
        if let Some(prompt) = prompt {
            spawn(async move {
                if let Ok(list) = prompt.manager.list_prompts().await {
                    let names: Vec<String> = list
                        .into_iter()
                        .map(|info| info.name)
                        .filter(|n| n.starts_with("chat/"))
                        .collect();
                    templates.set(names);
                }
            });
        }
    });

    let sidx = chat.sidx();
    let current_template = template.read().clone().unwrap_or_default();

    rsx! {
        document::Stylesheet { href: CHAT_CSS }
        document::Stylesheet { href: PLAN_CSS }
        div { class: "chat-test-page",

            // ═══════════════════════════════════════════════════════
            // 顶栏
            // ═══════════════════════════════════════════════════════
            header { class: "chat-test-topbar",
                div { class: "chat-test-topbar__left",
                    span { class: "chat-test-topbar__logo", "⚡" }
                    h1 { class: "chat-test-topbar__title", "Chat 测试" }
                    span { class: "chat-test-topbar__subtitle", "request_user_action 调试台" }
                }
                div { class: "chat-test-topbar__right",
                    span { class: "chat-test-topbar__template", title: "当前 system prompt 模板", "{current_template}" }
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        onclick: move |_| on_back.call(()),
                        "← 返回"
                    }
                }
            }

            // ═══════════════════════════════════════════════════════
            // 消息列表（内嵌 request_user_action 卡片）
            // ═══════════════════════════════════════════════════════
            div { class: "chat-messages",
                ScrollArea {
                    div { class: "chat-messages__list",
                        for (idx, msg) in chat.messages.read().iter().enumerate() {
                            {
                                let is_streaming = sidx == Some(idx);
                                let text = display_text(msg);
                                let class = format!(
                                    "chat-message chat-message--{} {}",
                                    role_css_class(&msg.role),
                                    if is_streaming { "chat-message--streaming" } else { "" }
                                );
                                let r_text: String = chat
                                    .reasoning_texts
                                    .read()
                                    .get(idx)
                                    .and_then(|o| o.clone())
                                    .unwrap_or_default();
                                let has_reasoning = !r_text.is_empty();
                                let show_streaming_cursor =
                                    is_streaming && text.is_empty() && !has_reasoning;
                                rsx! {
                                    div { class: "{class}",
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

                        // 内嵌 request_user_action 交互卡片（位于消息流末尾）
                        if let Some(pending) = chat.pending_ui.read().as_ref() {
                            {
                                let p = pending.clone();
                                let chat = chat;
                                let chat_signal = chat_signal;
                                rsx! {
                                    div { class: "chat-message chat-message--interaction",
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
                                                );
                                            },
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ═══════════════════════════════════════════════════════
            // 输入区 + 提示词选择器
            // ═══════════════════════════════════════════════════════
            div { class: "chat-input-area",
                div { class: "chat-input-row",
                    Textarea {
                        placeholder: "输入消息...（发送 \"开始\" 触发 request_user_action 演示）",
                        value: "{chat.input_text}",
                        disabled: sidx.is_some(),
                        oninput: move |e: FormEvent| chat.input_text.set(e.value()),
                        onkeydown: move |e: KeyboardEvent| {
                            if e.data.key() == keyboard_types::Key::Enter
                                && !e.data.modifiers().shift()
                            {
                                e.prevent_default();
                                if sidx.is_none() {
                                    let text = chat.input_text.read().trim().to_string();
                                    if !text.is_empty() {
                                        send_message(chat_signal, chat, text);
                                    }
                                }
                            }
                        },
                    }
                    div { class: "chat-input-row__actions",
                        if sidx.is_none() {
                            Button {
                                variant: ButtonVariant::Primary,
                                size: ButtonSize::Sm,
                                onclick: move |_| {
                                    let text = chat.input_text.read().trim().to_string();
                                    if !text.is_empty() {
                                        send_message(chat_signal, chat, text);
                                    }
                                },
                                "发送"
                            }
                        } else {
                            Button {
                                variant: ButtonVariant::Destructive,
                                size: ButtonSize::Sm,
                                onclick: move |_| {
                                    if let Some(ref svc) = *chat_signal.read() {
                                        svc.stop();
                                    }
                                },
                                "⏹ 停止"
                            }
                        }
                    }
                }
                // 工具栏：左侧动态提示词选择器，右侧清空对话
                div { class: "chat-toolbar",
                    div { class: "chat-toolbar__left",
                        span { class: "chat-toolbar__label", "提示词模板:" }
                        select {
                            class: "chat-toolbar__select",
                            value: "{current_template}",
                            onchange: move |e: FormEvent| {
                                let v = e.value();
                                if !v.is_empty() {
                                    // 切换模板会重建 ChatService；若正在流式，先停止当前服务，
                                    // 避免旧 stream 继续写消息却无法被「停止」按钮终止
                                    if let Some(ref svc) = *chat_signal.read() {
                                        svc.stop();
                                    }
                                    template.set(Some(v));
                                }
                            },
                            if templates.read().is_empty() {
                                option { value: "{current_template}", "{current_template}" }
                            } else {
                                for name in templates.read().iter() {
                                    option { value: "{name}", "{name}" }
                                }
                            }
                        }
                    }
                    div { class: "chat-toolbar__right",
                        Button {
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::Sm,
                            disabled: sidx.is_some(),
                            title: "清空对话（内存态，不持久化）",
                            onclick: move |_| chat.clear(),
                            "清空对话"
                        }
                    }
                }
            }
        }
    }
}

/// 从 `core::Message` 取出可显示文本（仅 `MessageContent::Text`；其他变体视为空串）。
fn display_text(msg: &Message) -> &str {
    match &msg.content {
        Some(MessageContent::Text { text }) => text.as_str(),
        _ => "",
    }
}

/// `MessageRole` → UI CSS class。
fn role_css_class(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Tool => "tool",
    }
}
