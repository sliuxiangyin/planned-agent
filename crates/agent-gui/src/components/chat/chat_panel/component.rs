//! 通用聊天面板组件：消息列表 + 输入区 + composer 工具栏。
//!
//! 这是一个纯 UI 组件，封装了完整的聊天界面渲染：
//! - 消息列表（含 ReasoningView + ToolView + Markdown）
//! - PendingUI 交互卡片
//! - 输入区（Textarea + 发送/停止按钮）
//! - Composer 工具栏（模板选择、思考模式、温度选择）
//! - 清空会话二次确认弹窗
//!
//! 业务逻辑（ChatFlow、ChatService 初始化）由调用方在页面层处理。
//! 气泡数据由 `ChatSignals` 维护（`bubbles` 历史 + `active` 当前 turn），
//! 本组件只读、只渲染。

use dioxus::prelude::*;

use dioxus_icons::lucide::{ArrowUp, Brain, ChevronDown, FileText, Square, Thermometer};

use crate::components::alert_dialog::{
    AlertDialog, AlertDialogAction, AlertDialogActions, AlertDialogCancel, AlertDialogDescription,
    AlertDialogTitle,
};
use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::chat_flow::{send_message, AgentViewData, Bubble, ChatSignals, PendingUI};
use crate::components::chat::chat_ui_actions_view::ChatUIActionsView;
use crate::components::chat::reasoning_view::ReasoningView;
use crate::components::chat::tool_view::ToolView;
use crate::components::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use crate::components::scroll_area::ScrollArea;
use crate::components::textarea::Textarea;
use dioxus_primitives::tooltip::{
    Tooltip as TooltipPrim, TooltipContent as TooltipContentPrim,
    TooltipTrigger as TooltipTriggerPrim,
};
use planned_agent::ChatService;
use planned_agent_prompt_manager::FilePromptManager;
use std::sync::Arc;

#[css_module("/src/components/chat/chat_panel/style.css")]
struct Styles;

// ── Props ──────────────────────────────────────────────────────────────────

/// 完整聊天面板 Props。
#[derive(Props, Clone)]
pub struct ChatPanelProps {
    pub chat: ChatSignals,
    pub chat_service: Arc<ChatService<FilePromptManager>>,
    pub on_user_action: EventHandler<(planned_agent::UIAction, String, PendingUI)>,
    #[props(default = String::new())]
    pub template_label: String,
    #[props(default)]
    pub templates: Vec<String>,
    #[props(default = None)]
    pub on_template_change: Option<EventHandler<String>>,
    #[props(default = true)]
    pub thinking: bool,
    #[props(default = None)]
    pub on_thinking_change: Option<EventHandler<bool>>,
    #[props(default = String::from("0.7"))]
    pub temperature: String,
    #[props(default = vec!["0.2".into(), "0.4".into(), "0.6".into(), "0.8".into(), "1.0".into()])]
    pub temperatures: Vec<String>,
    #[props(default = None)]
    pub on_temperature_change: Option<EventHandler<String>>,
    pub on_clear: EventHandler<()>,
}

/// 手动实现 PartialEq：`Arc<ChatService>` 不实现 PartialEq，
/// 用指针相等性判断（同一实例即相等）。
impl PartialEq for ChatPanelProps {
    fn eq(&self, other: &Self) -> bool {
        self.chat == other.chat
            && Arc::ptr_eq(&self.chat_service, &other.chat_service)
            && self.template_label == other.template_label
            && self.templates == other.templates
            && self.thinking == other.thinking
            && self.temperature == other.temperature
            && self.temperatures == other.temperatures
    }
}

// ── 组件 ──────────────────────────────────────────────────────────────────

/// 完整聊天面板组件。
#[component]
pub fn ChatPanel(props: ChatPanelProps) -> Element {
    let chat = props.chat;
    let svc = props.chat_service.clone();

    let mut show_clear_dialog = use_signal_sync(|| false);

    let is_streaming = chat.is_streaming();
    let has_pending = chat.has_pending();
    let busy = is_streaming || has_pending;

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

    use_effect(move || {
        let _len = chat.bubbles.read().len();
        let _active = chat.active.read().len();
        let _has_pending = chat.pending_ui.read().is_some();
        let _ = document::eval(
            "setTimeout(() => { const el = document.getElementById('chat-scroll'); if (el) el.scrollTop = el.scrollHeight; }, 100);"
        );
    });

    let bubbles = chat.bubbles.read();
    let active = chat.active.read();
    let agent_views = chat.agent_views.read();

    rsx! {
        div { class: Styles::flexible_page,

            // ═══════════════════════════════════════════════════════
            // 消息列表
            // ═══════════════════════════════════════════════════════
            div { class: Styles::chat_messages,
                ScrollArea {
                    id: "chat-scroll",
                    div { class: Styles::chat_messages__list,

                        for bubble in bubbles.iter().chain(active.iter()) {
                            if bubble.is_assistant {
                                { render_assistant_bubble(bubble, &agent_views) }
                            } else {
                                { render_user_bubble(bubble) }
                            }
                        }

                        if let Some(pending) = chat.pending_ui.read().as_ref() {
                            {
                                let p = pending.clone();
                                let on_action = props.on_user_action.clone();
                                let cls = format!("{} {}", Styles::chat_message, Styles::chat_message__interaction);
                                rsx! {
                                    div { class: "{cls}",
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
            // 输入区
            // ═══════════════════════════════════════════════════════
            { render_composer(
                busy, chat, svc.clone(),
                &props.template_label, props.templates.clone(),
                props.thinking, props.temperature.clone(), props.temperatures.clone(),
                props.on_thinking_change.clone(), props.on_temperature_change.clone(),
                apply_template,
            ) }
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

// ── 气泡渲染 ──────────────────────────────────────────────────────────────

fn render_assistant_bubble(bubble: &Bubble, agent_views: &std::collections::HashMap<String, AgentViewData>) -> Element {
    let bubble_class = if bubble.is_streaming {
        format!(
            "{} {} {}",
            Styles::chat_message,
            Styles::chat_message__assistant,
            Styles::chat_message__streaming
        )
    } else {
        format!(
            "{} {}",
            Styles::chat_message,
            Styles::chat_message__assistant
        )
    };

    rsx! {
        div { class: "{bubble_class}",
            { render_assistant_message(bubble, agent_views) }
        }
    }
}

fn render_assistant_message(msg: &Bubble, agent_views: &std::collections::HashMap<String, AgentViewData>) -> Element {
    let has_reasoning = !msg.reasoning.is_empty();
    // 有工具调用进行中时用 ToolView 的 Pending/Running 动画代替光标
    let show_cursor =
        msg.is_streaming && msg.text.is_empty() && !has_reasoning && msg.tool_calls.is_empty();

    rsx! {
        if has_reasoning {
            ReasoningView { text: msg.reasoning.clone(), is_streaming: msg.is_streaming }
        }
        // 文本在 ToolView 之前显示（与 ChatGPT / Claude / Cursor 一致）：
        // assistant 通常先说一段话，再发起工具调用；工具面板紧随其说明文本之后
        if show_cursor {
            "▍"
        } else if !msg.text.is_empty() {
            crate::components::markdown::Markdown { text: msg.text.clone() }
        }
        for entry in msg.tool_calls.iter() {
            if entry.is_sub_agent {
                if let Some(av) = agent_views.get(&entry.tool_call_id) {
                    crate::components::chat::agent_view::AgentView { data: av.clone() }
                }
            } else {
                ToolView { entry: entry.clone() }
            }
        }
    }
}

fn render_user_bubble(bubble: &Bubble) -> Element {
    let cls = format!("{} {}", Styles::chat_message, Styles::chat_message__user);

    rsx! {
        div { class: "{cls}",
            crate::components::markdown::Markdown { text: bubble.text.clone() }
        }
    }
}

// ── 输入区渲染 ────────────────────────────────────────────────────────────

/// 渲染 composer。所有 Props 数据由调用方 clone 后传入，避免引用逃逸到闭包。
fn render_composer(
    busy: bool,
    mut chat: ChatSignals,
    svc: Arc<ChatService<FilePromptManager>>,
    template_label: &str,
    templates: Vec<String>,
    thinking: bool,
    temperature: String,
    temperatures: Vec<String>,
    on_thinking_change: Option<EventHandler<bool>>,
    on_temperature_change: Option<EventHandler<String>>,
    apply_template: impl Fn(String) + Clone + 'static,
) -> Element {
    let svc_send = svc.clone();
    let svc_stop = svc.clone();
    let svc_key = svc.clone();
    rsx! {
        div { class: Styles::chat_composer,
            Textarea {
                class: Styles::chat_composer__textarea,
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
                                send_message(&mut chat, &svc_key, text);
                            }
                        }
                    }
                },
            }
            div { class: Styles::chat_composer__footer,
                div { class: Styles::chat_composer__tools,

                    // ── 模板选择 ──
                    if !templates.is_empty() {
                        TooltipPrim {
                            TooltipTriggerPrim {
                                DropdownMenu {
                                    class: Styles::chat_composer__tool,
                                    DropdownMenuTrigger {
                                        class: Styles::chat_composer__tool_btn,
                                        FileText { size: "14" }
                                        span { class: Styles::chat_composer__tool_label, "{template_label}" }
                                        ChevronDown { size: "14" }
                                    }
                                    DropdownMenuContent {
                                        for (idx, name) in templates.iter().enumerate() {
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
                            TooltipContentPrim { "{template_label}" }
                        }
                    }

                    // ── 思考模式 ──
                    TooltipPrim {
                        TooltipTriggerPrim {
                            button {
                                class: if thinking {
                                    format!("{} {}", Styles::chat_composer__tool_btn, Styles::chat_composer__tool_btn__active)
                                } else {
                                    Styles::chat_composer__tool_btn.to_string()
                                },
                                onclick: {
                                    let handler = on_thinking_change.clone();
                                    move |_| {
                                        if let Some(ref handler) = handler {
                                            handler.call(!thinking);
                                        }
                                    }
                                },
                                Brain { size: "14" }
                            }
                        }
                        TooltipContentPrim { "思考" }
                    }

                    // ── 温度选择 ──
                    TooltipPrim {
                        TooltipTriggerPrim {
                            DropdownMenu {
                                class: Styles::chat_composer__tool,
                                DropdownMenuTrigger {
                                    class: Styles::chat_composer__tool_btn,
                                    Thermometer { size: "14" }
                                    span { class: Styles::chat_composer__tool_label, "{temperature}" }
                                    ChevronDown { size: "14" }
                                }
                                DropdownMenuContent {
                                    for (idx, t) in temperatures.iter().enumerate() {
                                        DropdownMenuItem::<String> {
                                            value: t.clone(),
                                            index: idx,
                                            on_select: {
                                                let handler = on_temperature_change.clone();
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

                // ── 发送 / 停止 ──
                if !busy {
                    Button {
                        class: Styles::chat_composer__send,
                        variant: ButtonVariant::Primary,
                        size: ButtonSize::Icon,
                        title: "发送",
                        onclick: move |_| {
                            let text = chat.input_text.read().trim().to_string();
                            if !text.is_empty() {
                                send_message(&mut chat, &svc_send, text);
                            }
                        },
                        ArrowUp { size: "18" }
                    }
                } else {
                    Button {
                        class: Styles::chat_composer__send,
                        variant: ButtonVariant::Destructive,
                        size: ButtonSize::Icon,
                        title: "停止",
                        onclick: move |_| {
                            svc_stop.stop();
                        },
                        Square { size: "16" }
                    }
                }
            }
        }
    }
}

// ── 工具函数 ──────────────────────────────────────────────────────────────

/// 模板路径 → 短标签。
pub fn template_label(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}
