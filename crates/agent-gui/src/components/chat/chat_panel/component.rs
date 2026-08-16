//! 通用聊天面板组件：消息列表 + 输入区 + composer 工具栏。
//!
//! 这是一个纯 UI 组件，封装了完整的聊天界面渲染：
//! - 消息列表（含 ReasoningView + Markdown）
//! - PendingUI 交互卡片
//! - 输入区（Textarea + 发送/停止按钮）
//! - Composer 工具栏（模板选择、思考模式、温度选择）
//! - 清空会话二次确认弹窗
//!
//! 业务逻辑（ChatFlow、V2ChatService 初始化）由调用方在页面层处理。

use dioxus::prelude::*;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use dioxus_icons::lucide::{ArrowUp, Brain, ChevronDown, FileText, Square, Thermometer};

use crate::components::alert_dialog::{
    AlertDialog, AlertDialogAction, AlertDialogActions, AlertDialogCancel, AlertDialogDescription,
    AlertDialogTitle,
};
use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::chat_flow::{ChatSignals, PendingUI};
use crate::components::chat::chat_ui_actions_view::ChatUIActionsView;
use crate::components::chat::reasoning_view::ReasoningView;
use crate::components::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use dioxus_primitives::tooltip::{Tooltip as TooltipPrim, TooltipContent as TooltipContentPrim, TooltipTrigger as TooltipTriggerPrim};
use crate::components::scroll_area::ScrollArea;
use crate::components::textarea::Textarea;
use crate::services::chat_service::V2ChatServiceSignal;

use crate::components::chat::chat_flow::send_message;

/// 本组件专属样式（复用 plan.css 中的 .flexible-page 样式）。
const PLAN_CSS: Asset = asset!("/assets/plan.css");

/// 完整聊天面板 Props。
#[derive(Props, Clone, PartialEq)]
pub struct ChatPanelProps {
    /// 聊天状态信号（消息列表、输入框等）
    pub chat: ChatSignals,
    /// V2ChatService 信号（用于发送消息、停止等操作）
    pub chat_signal: V2ChatServiceSignal,
    /// 用户操作回调（处理 UIAction 卡片交互）
    pub on_user_action: EventHandler<(planned_agent::chat::UIAction, String, PendingUI)>,
    /// 模板标签（显示在 composer 工具栏）
    #[props(default = String::new())]
    pub template_label: String,
    /// 可用模板列表（空 = 不显示模板选择器）
    #[props(default)]
    pub templates: Vec<String>,
    /// 模板切换回调
    #[props(default = None)]
    pub on_template_change: Option<EventHandler<String>>,
    /// 是否启用思考模式
    #[props(default = true)]
    pub thinking: bool,
    /// 思考模式切换回调
    #[props(default = None)]
    pub on_thinking_change: Option<EventHandler<bool>>,
    /// 温度值（字符串形式，如 "0.7"）
    #[props(default = String::from("0.7"))]
    pub temperature: String,
    /// 可选温度值列表
    #[props(default = vec!["0.2".into(), "0.4".into(), "0.6".into(), "0.8".into(), "1.0".into()])]
    pub temperatures: Vec<String>,
    /// 温度切换回调
    #[props(default = None)]
    pub on_temperature_change: Option<EventHandler<String>>,
    /// 清空会话回调
    pub on_clear: EventHandler<()>,
}

/// 完整聊天面板组件。
///
/// 封装了消息列表、输入区、composer 工具栏和清空确认弹窗。
/// 调用方只需传入信号和回调，无需关心 UI 渲染细节。
#[component]
pub fn ChatPanel(props: ChatPanelProps) -> Element {
    let mut chat = props.chat;
    let chat_signal = props.chat_signal;

    // ── 清空会话二次确认弹窗 ──
    let mut show_clear_dialog = use_signal_sync(|| false);

    let sidx = chat.sidx();
    let has_pending = chat.has_pending();
    let busy = sidx.is_some() || has_pending;

    // ── 模板切换回调 ──
    let apply_template = {
        let on_template_change = props.on_template_change.clone();
        move |name: String| {
            if name.is_empty() {
                return;
            }
            if let Some(ref handler) = on_template_change {
                handler.call(name);
            }
        }
    };

    // ── 自动滚到底部：追踪消息列表和 pending UI 变化 ──
    use_effect(move || {
        let _len = chat.messages.read().len();
        let _has_pending = chat.pending_ui.read().is_some();
        let _ = document::eval(
            "setTimeout(() => { const el = document.getElementById('chat-scroll'); if (el) el.scrollTop = el.scrollHeight; }, 100);"
        );
    });

    rsx! {
        document::Stylesheet { href: PLAN_CSS }
        div { class: "flexible-page",

            // ═══════════════════════════════════════════════════════
            // 消息列表
            // ═══════════════════════════════════════════════════════
            div { class: "chat-messages",
                ScrollArea {
                    id: "chat-scroll",
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
                                            crate::components::markdown::Markdown { text: text.to_string() }
                                        }
                                    }
                                }
                            }
                        }

                        // 内嵌 request_user_action / 子 agent 交互卡片
                        if let Some(pending) = chat.pending_ui.read().as_ref() {
                            {
                                let p = pending.clone();
                                let chat_sig = chat_signal;
                                let chat_clone = chat;
                                let on_action = props.on_user_action.clone();
                                rsx! {
                                    div { class: "chat-message chat-message--interaction",
                                        ChatUIActionsView {
                                            message: p.message.clone(),
                                            actions: p.actions.clone(),
                                            on_action: move |(action, choice)| {
                                                on_action.call((action, choice, p.clone()));
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
            // 输入区（DeepSeek Web 风格 composer：textarea + 底部工具行）
            // ═══════════════════════════════════════════════════════
            div { class: "chat-composer",
                Textarea {
                    placeholder: "输入消息...",
                    value: "{chat.input_text}",
                    disabled: busy,
                    oninput: move |e: FormEvent| chat.input_text.set(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.data.key() == keyboard_types::Key::Enter
                            && !e.data.modifiers().shift()
                        {
                            e.prevent_default();
                            if !busy {
                                let text = chat.input_text.read().trim().to_string();
                                if !text.is_empty() {
                                    send_message(chat_signal, chat, text);
                                }
                            }
                        }
                    },
                }
                div { class: "chat-composer__footer",
                    div { class: "chat-composer__tools",

                        // ── 模板选择（图标 + 文字 + 下拉）──
                        if !props.templates.is_empty() {
                            TooltipPrim {
                                TooltipTriggerPrim {
                                    DropdownMenu {
                                        class: "chat-composer__tool",
                                        DropdownMenuTrigger {
                                            class: "chat-composer__tool-btn",
                                            FileText { size: "14" }
                                            span { class: "chat-composer__tool-label", "{props.template_label}" }
                                            ChevronDown { size: "14" }
                                        }
                                        DropdownMenuContent {
                                            class: "chat-composer__tool-menu",
                                            for (idx, name) in props.templates.iter().enumerate() {
                                                DropdownMenuItem::<String> {
                                                    value: name.clone(),
                                                    index: idx,
                                                    on_select: apply_template.clone(),
                                                    "{name}"
                                                }
                                            }
                                        }
                                    }
                                }
                                TooltipContentPrim { "{props.template_label}" }
                            }
                        }

                        // ── 思考模式 chip（纯图标 + Tooltip）──
                        TooltipPrim {
                            TooltipTriggerPrim {
                                button {
                                    class: if props.thinking {
                                        "chat-composer__tool-btn chat-composer__tool-btn--active"
                                    } else {
                                        "chat-composer__tool-btn"
                                    },
                                    onclick: {
                                        let handler = props.on_thinking_change.clone();
                                        move |_| {
                                            if let Some(ref handler) = handler {
                                                handler.call(!props.thinking);
                                            }
                                        }
                                    },
                                    Brain { size: "14" }
                                }
                            }
                            TooltipContentPrim { "思考" }
                        }

                        // ── 温度 chip（图标 + 数值 + 下拉 + Tooltip）──
                        TooltipPrim {
                            TooltipTriggerPrim {
                                DropdownMenu {
                                    class: "chat-composer__tool",
                                    DropdownMenuTrigger {
                                        class: "chat-composer__tool-btn",
                                        Thermometer { size: "14" }
                                        span { class: "chat-composer__tool-label", "{props.temperature}" }
                                        ChevronDown { size: "14" }
                                    }
                                    DropdownMenuContent {
                                        class: "chat-composer__tool-menu",
                                        for (idx, t) in props.temperatures.iter().enumerate() {
                                            DropdownMenuItem::<String> {
                                                value: t.clone(),
                                                index: idx,
                                                on_select: {
                                                    let handler = props.on_temperature_change.clone();
                                                    move |v: String| {
                                                        if let Some(ref handler) = handler {
                                                            handler.call(v);
                                                        }
                                                    }
                                                },
                                                "{t}"
                                            }
                                        }
                                    }
                                }
                            }
                            TooltipContentPrim { "温度" }
                        }
                    }

                    // ── 发送 / 停止（圆形 icon 按钮）──
                    if !busy {
                        Button {
                            class: "chat-composer__send",
                            variant: ButtonVariant::Primary,
                            size: ButtonSize::Icon,
                            title: "发送",
                            onclick: move |_| {
                                let text = chat.input_text.read().trim().to_string();
                                if !text.is_empty() {
                                    send_message(chat_signal, chat, text);
                                }
                            },
                            ArrowUp { size: "18" }
                        }
                    } else {
                        Button {
                            class: "chat-composer__send",
                            variant: ButtonVariant::Destructive,
                            size: ButtonSize::Icon,
                            title: "停止",
                            onclick: move |_| {
                                if let Some(ref svc) = *chat_signal.read() {
                                    svc.stop();
                                }
                            },
                            Square { size: "16" }
                        }
                    }
                }
            }
        }

        // ═══════════════════════════════════════════════════════════
        // 清空会话二次确认弹窗
        // ═══════════════════════════════════════════════════════════
        AlertDialog {
            open: show_clear_dialog(),
            on_open_change: move |v: bool| show_clear_dialog.set(v),
            AlertDialogTitle { "清空会话？" }
            AlertDialogDescription {
                "确定要清空当前会话的全部消息吗？此操作不可撤销。"
            }
            AlertDialogActions {
                AlertDialogCancel { "取消" }
                AlertDialogAction {
                    on_click: move |_| {
                        show_clear_dialog.set(false);
                        props.on_clear.call(());
                    },
                    "清空"
                }
            }
        }
    }
}

/// 模板路径 → 短标签（取最后一段，用于 chip 展示）。
pub fn template_label(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// 从 `Message` 取出可显示文本。
fn display_text(msg: &Message) -> &str {
    match &msg.content {
        Some(MessageContent::Text { text }) => text.as_str(),
        _ => "",
    }
}

/// `MessageRole` → CSS class。
fn role_css_class(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Tool => "tool",
    }
}
