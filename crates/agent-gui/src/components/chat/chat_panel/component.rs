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

use std::collections::HashMap;

use dioxus::prelude::*;
use planned_agent_core::ai::types::{MessageContent, MessageRole};

use dioxus_icons::lucide::{ArrowUp, Brain, ChevronDown, FileText, Square, Thermometer};

use crate::components::alert_dialog::{
    AlertDialog, AlertDialogAction, AlertDialogActions, AlertDialogCancel, AlertDialogDescription,
    AlertDialogTitle,
};
use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::chat_flow::{
    send_message, ChatMessage, ChatSignals, PendingUI, ToolCallPhase, ToolViewData,
};
use crate::components::chat::chat_ui_actions_view::ChatUIActionsView;
use crate::components::chat::reasoning_view::ReasoningView;
use crate::components::chat::tool_view::ToolView;
use crate::components::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use crate::components::scroll_area::ScrollArea;
use crate::components::textarea::Textarea;
use crate::services::chat_service::ChatServiceSignal;
use dioxus_primitives::tooltip::{
    Tooltip as TooltipPrim, TooltipContent as TooltipContentPrim,
    TooltipTrigger as TooltipTriggerPrim,
};

#[css_module("/src/components/chat/chat_panel/style.css")]
struct Styles;

// ── 渲染数据结构（预计算，rsx 只负责布局）─────────────────────────────────

/// 单条消息的渲染数据。
#[derive(Clone)]
struct RenderMessage {
    text: String,
    reasoning: String,
    is_streaming: bool,
    tool_calls: Vec<ToolViewData>,
}

/// 一组气泡的渲染数据（连续同角色消息合并为一个气泡）。
#[derive(Clone)]
struct RenderBubble {
    is_assistant: bool,
    messages: Vec<RenderMessage>,
    is_streaming: bool,
}

/// 从 `ChatMessage` 列表构建渲染气泡。
///
/// 严格按 OpenAI 消息序列渲染：`User → Assistant(tool_calls) → Tool → Assistant(text)`。
/// - User → 独立 user 气泡
/// - Assistant → 独立 assistant 气泡（reasoning + ToolView + text），
///   无论是否携带 tool_calls，text 都保留渲染（assistant 可边说边调用工具）
/// - Tool → 不产生气泡，按 `tool_call_id` 回填到对应 Assistant 的 ToolViewData.result
fn build_bubbles(chat_msgs: &[ChatMessage]) -> Vec<RenderBubble> {
    tracing::info!(target: "render", "build_bubbles: 输入 {} 条消息, roles={:?}", chat_msgs.len(),
        chat_msgs.iter().map(|m| format!("{}:{:?}", m.sequence_order, m.message.role)).collect::<Vec<_>>()
    );
    let mut bubbles: Vec<RenderBubble> = Vec::new();
    // tool_call_id → (bubble 索引, msg 索引, entry 索引)，供 Tool 消息精确回填 result
    let mut tool_index: HashMap<String, (usize, usize, usize)> = HashMap::new();

    for cm in chat_msgs {
        match cm.message.role {
            MessageRole::User => {
                bubbles.push(RenderBubble {
                    is_assistant: false,
                    messages: vec![to_render_message(cm)],
                    is_streaming: false,
                });
            }
            // Assistant：独立气泡，同时渲染 reasoning / tool_calls / text
            MessageRole::Assistant => {
                let mut render_msg = to_render_message(cm);
                render_msg.tool_calls = resolve_tool_entries(cm);
                let bubble_idx = bubbles.len();
                for (entry_idx, entry) in render_msg.tool_calls.iter().enumerate() {
                    tool_index.insert(entry.tool_call_id.clone(), (bubble_idx, 0, entry_idx));
                }
                bubbles.push(RenderBubble {
                    is_assistant: true,
                    messages: vec![render_msg],
                    is_streaming: cm.is_streaming,
                });
            }
            // Tool：按 tool_call_id 回填 result，不产生独立气泡
            MessageRole::Tool => {
                if let Some(id) = cm.tool_call_id.as_deref() {
                    if let Some(&(b, m, e)) = tool_index.get(id) {
                        if let Some(result) = parse_tool_result(cm) {
                            bubbles[b].messages[m].tool_calls[e].result = Some(result);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    bubbles
}

/// 组合一条消息的 ToolView 渲染数据。
///
/// `name`/`arguments` 以 `Message.tool_calls` 为权威；
/// `phase`/`result`/`is_error` 取自实时 `tool_call_entries`（按 tool_call_id 关联，
/// 无则视为 Completed —— 历史加载后无实时状态）。
fn resolve_tool_entries(cm: &ChatMessage) -> Vec<ToolViewData> {
    let Some(tcs) = &cm.message.tool_calls else {
        return Vec::new();
    };
    tcs.iter()
        .map(|tc| {
            let state = cm
                .tool_call_entries
                .iter()
                .find(|e| e.tool_call_id == tc.id);
            ToolViewData {
                tool_call_id: tc.id.clone(),
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
                phase: state
                    .map(|s| s.phase.clone())
                    .unwrap_or(ToolCallPhase::Completed),
                result: state.and_then(|s| s.result.clone()),
                is_error: state.map(|s| s.is_error).unwrap_or(false),
            }
        })
        .collect()
}

/// 解析 Tool 消息的 content（JSON 字符串）为结果值。
fn parse_tool_result(cm: &ChatMessage) -> Option<serde_json::Value> {
    let content_text = cm.message.content.as_ref().and_then(|c| {
        if let MessageContent::Text { text } = c { Some(text.as_str()) } else { None }
    })?;
    Some(
        serde_json::from_str(content_text)
            .unwrap_or_else(|_| serde_json::Value::String(content_text.to_string())),
    )
}

/// 将 `ChatMessage` 转换为渲染数据。
fn to_render_message(cm: &ChatMessage) -> RenderMessage {
    RenderMessage {
        text: display_text(&cm.message).to_string(),
        reasoning: cm.message.reasoning_content.clone().unwrap_or_default(),
        is_streaming: cm.is_streaming,
        tool_calls: Vec::new(), // 由 build_bubbles 填充
    }
}

// ── Props ──────────────────────────────────────────────────────────────────

/// 完整聊天面板 Props。
#[derive(Props, Clone, PartialEq)]
pub struct ChatPanelProps {
    pub chat: ChatSignals,
    pub chat_service_signal: ChatServiceSignal,
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

// ── 组件 ──────────────────────────────────────────────────────────────────

/// 完整聊天面板组件。
#[component]
pub fn ChatPanel(props: ChatPanelProps) -> Element {
    let chat = props.chat;
    let chat_service_signal = props.chat_service_signal;

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
        let _len = chat.messages.read().len();
        let _has_pending = chat.pending_ui.read().is_some();
        let _ = document::eval(
            "setTimeout(() => { const el = document.getElementById('chat-scroll'); if (el) el.scrollTop = el.scrollHeight; }, 100);"
        );
    });

    let bubbles = build_bubbles(&chat.messages.read());

    rsx! {
        div { class: Styles::flexible_page,

            // ═══════════════════════════════════════════════════════
            // 消息列表
            // ═══════════════════════════════════════════════════════
            div { class: Styles::chat_messages,
                ScrollArea {
                    id: "chat-scroll",
                    div { class: Styles::chat_messages__list,

                        for bubble in bubbles.iter() {
                            if bubble.is_assistant {
                                { render_assistant_bubble(bubble) }
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
                busy, chat, chat_service_signal,
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

fn render_assistant_bubble(bubble: &RenderBubble) -> Element {
    let bubble_class = if bubble.is_streaming {
        format!("{} {} {}", Styles::chat_message, Styles::chat_message__assistant, Styles::chat_message__streaming)
    } else {
        format!("{} {}", Styles::chat_message, Styles::chat_message__assistant)
    };

    rsx! {
        div { class: "{bubble_class}",
            for msg in bubble.messages.iter() {
                { render_assistant_message(msg) }
            }
        }
    }
}

fn render_assistant_message(msg: &RenderMessage) -> Element {
    let has_reasoning = !msg.reasoning.is_empty();
    // 有工具调用进行中时用 ToolView 的 Pending/Running 动画代替光标
    let show_cursor = msg.is_streaming && msg.text.is_empty() && !has_reasoning && msg.tool_calls.is_empty();

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
            ToolView { entry: entry.clone() }
        }
    }
}

fn render_user_bubble(bubble: &RenderBubble) -> Element {
    let cls = format!("{} {}", Styles::chat_message, Styles::chat_message__user);

    rsx! {
        for msg in bubble.messages.iter() {
            div { class: "{cls}",
                crate::components::markdown::Markdown { text: msg.text.clone() }
            }
        }
    }
}

// ── 输入区渲染 ────────────────────────────────────────────────────────────

/// 渲染 composer。所有 Props 数据由调用方 clone 后传入，避免引用逃逸到闭包。
fn render_composer(
    busy: bool,
    mut chat: ChatSignals,
    chat_service_signal: ChatServiceSignal,
    template_label: &str,
    templates: Vec<String>,
    thinking: bool,
    temperature: String,
    temperatures: Vec<String>,
    on_thinking_change: Option<EventHandler<bool>>,
    on_temperature_change: Option<EventHandler<String>>,
    apply_template: impl Fn(String) + Clone + 'static,
) -> Element {
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
                                send_message(&mut chat, chat_service_signal, text);
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
                                send_message(&mut chat, chat_service_signal, text);
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
                            if let Some(ref svc) = *chat_service_signal.read() {
                                svc.stop();
                            }
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

/// 从 `Message` 取出可显示文本。
fn display_text(msg: &planned_agent_core::ai::types::Message) -> &str {
    match &msg.content {
        Some(planned_agent_core::ai::types::MessageContent::Text { text }) => text.as_str(),
        _ => "",
    }
}
