//! 灵活模式主组件：消息列表 + 输入区 + 提示词选择器。
//!
//! 基于 v2_chat 实现，支持子 agent 调用。
//! 子 agent 使用 `chat/request_user_action_demo` prompt，测试 UI 交互。

use std::sync::{Arc, OnceLock};

use dioxus::prelude::*;
use planned_agent::v2_chat::{V2ChatConfig, V2ChatSubAgentRunner};
use planned_agent::V2ChatService;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::prompt::PromptManager;
use planned_agent_tool_manager::ToolCategory;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::markdown::Markdown;
use crate::components::scroll_area::ScrollArea;
use crate::components::textarea::Textarea;
use crate::context::{AiContext, PromptContext, ToolsContext};
use crate::pages::plan::components::chat_ui_actions_view::ChatUIActionsView;
use crate::pages::plan::components::reasoning_view::ReasoningView;
use crate::services::chat_service::{use_v2_chat_service, V2ChatServiceSignal};

use super::chat_flow::{ensure_subscription, handle_user_action, send_message, ChatSignals, PendingUI};

/// 子 agent 注册幂等锁（进程内只注册一次）
static SUB_AGENT_REGISTERED: OnceLock<()> = OnceLock::new();

/// 子 agent 工具名称
const SUB_AGENT_TOOL_NAME: &str = "demo_agent";

/// 子 agent 使用的 prompt 模板
const SUB_AGENT_PROMPT: &str = "chat/request_user_action_demo";

/// 本页面自定义样式（复用 plan.css 中的 .flexible-page 样式）。
const PLAN_CSS: Asset = asset!("/assets/plan.css");

/// 默认父 agent 的 system prompt 模板（灵活模式不指定固定 prompt）
const DEFAULT_TEMPLATE: &str = "thorough/thorough_system";

#[component]
pub fn FlexiblePage() -> Element {
    // ── 纯内存聊天状态 ──
    let messages = use_signal_sync(|| Vec::<Message>::new());
    let reasoning_texts = use_signal_sync(|| Vec::<Option<String>>::new());
    let streaming_idx = use_signal_sync(|| None::<usize>);
    let pending_ui = use_signal_sync(|| None::<PendingUI>);
    let input_text = use_signal_sync(String::new);
    let pending_tool_call_id = use_signal_sync(|| None::<String>);
    let subscription = use_signal_sync(|| None::<planned_agent::v2_chat::SubscriptionGuard>);
    let mut chat = ChatSignals {
        messages,
        reasoning_texts,
        streaming_idx,
        pending_ui,
        input_text,
        pending_tool_call_id,
        subscription,
    };

    // ── 父 agent V2ChatService ──
    // 灵活模式：父 agent 仅暴露子 agent 工具，不暴露 request_user_action
    let mut template = use_signal_sync(|| Some(DEFAULT_TEMPLATE.to_string()));
    let initial_config = V2ChatConfig {
        system_prompt_template: template.read().clone(),
        allowed_tools: Some(vec![SUB_AGENT_TOOL_NAME.to_string()]),
        ..Default::default()
    };
    let chat_signal: V2ChatServiceSignal = use_v2_chat_service(initial_config);

    // ── 事件订阅：service ready 后注册一次 ──
    {
        let svc_sig = chat_signal;
        let chat_for_sub = chat;
        use_effect(move || {
            if let Some(ref svc) = *svc_sig.read() {
                ensure_subscription(svc, chat_for_sub);
            }
        });
    }

    // ── 注册子 agent（幂等；仅在 ai/tools/prompt 全部就绪时执行一次）──
    let ai_resource = use_context::<Resource<Option<Arc<AiContext>>>>();
    let tools_resource = use_context::<Resource<Option<Arc<ToolsContext>>>>();
    let prompt_resource = use_context::<Resource<Option<Arc<PromptContext>>>>();
    let ai = ai_resource.read().as_ref().and_then(|x| x.clone());
    let tools_ctx = tools_resource.read().as_ref().and_then(|x| x.clone());
    let prompt = prompt_resource.read().as_ref().and_then(|x| x.clone());
    if let (Some(ai), Some(ctx), Some(prompt)) = (ai, tools_ctx, prompt) {
        SUB_AGENT_REGISTERED.get_or_init(|| {
            // 创建子 agent 的 V2ChatService（独立 prompt，全部工具可用）
            let child_service = match V2ChatService::new(
                (*ai.manager).clone(),
                ctx.registry.clone(),
                prompt.manager.clone(),
                V2ChatConfig {
                    system_prompt_template: Some(SUB_AGENT_PROMPT.to_string()),
                    ..Default::default()
                },
            ) {
                Ok(svc) => svc,
                Err(e) => {
                    tracing::error!("子 agent V2ChatService 创建失败: {}", e);
                    return;
                }
            };

            // 创建 Runner（depth=0, max_depth=3 防递归）
            let runner = V2ChatSubAgentRunner::new(child_service, 0, 3);

            // 注册为子 agent 工具
            let tool = planned_agent_core::mcp::types::Tool {
                name: SUB_AGENT_TOOL_NAME.to_string(),
                description: "交互演示子 agent：依次演示 confirm/select/input/multi_select 四种 UI 交互类型".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task": {
                            "type": "string",
                            "description": "任务描述（如：开始演示）"
                        }
                    },
                    "required": ["task"]
                }),
            };
            ctx.registry.register_sub_agent(
                tool,
                vec![ToolCategory::Utility],
                Arc::new(runner),
            );
            tracing::info!(
                "子 agent 已注册: {} (prompt={}, max_depth=3)",
                SUB_AGENT_TOOL_NAME,
                SUB_AGENT_PROMPT
            );
        });
    }

    // ── 可用模板列表 ──
    let mut templates = use_signal_sync(|| Vec::<String>::new());
    let prompt_resource2 = use_context::<Resource<Option<Arc<PromptContext>>>>();
    use_effect(move || {
        let prompt = prompt_resource2
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
                        .filter(|n| n.starts_with("chat/") || n.starts_with("flexible/"))
                        .collect();
                    templates.set(names);
                }
            });
        }
    });

    let sidx = chat.sidx();
    let has_pending = chat.has_pending();
    let busy = sidx.is_some() || has_pending;
    let current_template = template.read().clone().unwrap_or_default();

    rsx! {
        document::Stylesheet { href: PLAN_CSS }
        div { class: "flexible-page",

            // ═══════════════════════════════════════════════════════
            // 消息列表
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

                        // 内嵌 request_user_action / 子 agent 交互卡片
                        if let Some(pending) = chat.pending_ui.read().as_ref() {
                            {
                                let p = pending.clone();
                                let chat_sig = chat_signal;
                                let chat_clone = chat;
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
                                                    chat_clone,
                                                    chat_sig,
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
            // 输入区
            // ═══════════════════════════════════════════════════════
            div { class: "chat-input-area",
                div { class: "chat-input-row",
                    Textarea {
                        placeholder: "输入消息...（发送 \"开始\" 触发子 agent 交互演示）",
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
                    div { class: "chat-input-row__actions",
                        if !busy {
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
                // 工具栏：提示词选择器 + 清空对话
                div { class: "chat-toolbar",
                    div { class: "chat-toolbar__left",
                        span { class: "chat-toolbar__label", "父 agent 模板:" }
                        select {
                            class: "chat-toolbar__select",
                            value: "{current_template}",
                            onchange: move |e: FormEvent| {
                                let v = e.value();
                                if !v.is_empty() {
                                    if let Some(ref svc) = *chat_signal.read() {
                                        svc.stop();
                                        svc.set_system_prompt_template(Some(v.clone()));
                                        if let Err(e) = svc.reset_session() {
                                            tracing::error!("重置会话失败: {}", e);
                                        }
                                    }
                                    chat.clear_pending();
                                    chat.pending_tool_call_id.set(None);
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
                            disabled: busy,
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
