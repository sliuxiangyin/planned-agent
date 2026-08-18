//! 灵活模式主组件：基于 chat 的聊天面板（含子 agent 支持）。
//!
//! 使用通用 `ChatPanel` 组件渲染 UI，自身只负责：
//! - ChatService 初始化与事件订阅
//! - 子 agent 注册（demo_agent）
//! - 模板切换与可用模板列表

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::chat::ChatConfig;
use planned_agent_core::prompt::PromptManager;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::{
    chat_flow::{ensure_subscription, handle_user_action, ChatSignals, PendingUI},
    ChatPanel,
};
use crate::components::page_header::PageHeader;
use crate::context::PromptContext;
use crate::services::chat_service::{use_chat_service, use_sub_agent, ChatServiceSignal};

use dioxus_icons::lucide::Trash2;

/// 子 agent 工具名称
const SUB_AGENT_TOOL_NAME: &str = "demo_agent";

/// 子 agent 使用的 prompt 模板
const SUB_AGENT_PROMPT: &str = "chat/request_user_action_demo";

/// 默认父 agent 的 system prompt 模板
const DEFAULT_TEMPLATE: &str = "flexible/flexible_parent_control";

#[component]
pub fn FlexiblePage() -> Element {
    // ── 纯内存聊天状态 ──
    let messages = use_signal_sync(|| Vec::<planned_agent_core::ai::types::Message>::new());
    let reasoning_texts = use_signal_sync(|| Vec::<Option<String>>::new());
    let streaming_idx = use_signal_sync(|| None::<usize>);
    let pending_ui = use_signal_sync(|| None::<PendingUI>);
    let input_text = use_signal_sync(String::new);
    let pending_tool_call_id = use_signal_sync(|| None::<String>);
    let subscription = use_signal_sync(|| None::<planned_agent::chat::SubscriptionGuard>);
    let tool_call_entries = use_signal_sync(std::collections::HashMap::new);
    let mut chat = ChatSignals {
        messages,
        reasoning_texts,
        streaming_idx,
        pending_ui,
        input_text,
        pending_tool_call_id,
        subscription,
        tool_call_entries,
    };

    // ── option 栏状态 ──
    let mut thinking = use_signal_sync(|| true);
    let mut temperature = use_signal_sync(|| "0.7".to_string());

    // ── 父 agent ChatService ──
    let mut template = use_signal_sync(|| Some(DEFAULT_TEMPLATE.to_string()));
    let initial_config = ChatConfig {
        system_prompt_template: template.read().clone(),
        allowed_tools: Some(vec![SUB_AGENT_TOOL_NAME.to_string()]),
        ..Default::default()
    };
    let chat_signal: ChatServiceSignal = use_chat_service(initial_config);

    // ── 事件订阅：service ready 后注册一次 ──
    {
        let svc_sig = chat_signal;
        let chat_for_sub = chat;
        use_effect(move || {
            if let Some(ref svc) = *svc_sig.read() {
                let _ = svc.start_driver();
                ensure_subscription(svc, chat_for_sub);
            }
        });
    }

    // ── 注册子 agent（幂等；由 use_sub_agent 内部管理）──
    use_sub_agent(
        SUB_AGENT_TOOL_NAME,
        "交互演示子 agent：依次演示 confirm/select/input/multi_select 四种 UI 交互类型",
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "任务描述（如：开始演示）"
                }
            },
            "required": ["task"]
        }),
        ChatConfig {
            system_prompt_template: Some(SUB_AGENT_PROMPT.to_string()),
            ..Default::default()
        },
    );

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

    let current_template = template.read().clone().unwrap_or_default();
    let sidx = chat.sidx();
    let has_pending = chat.has_pending();
    let busy = sidx.is_some() || has_pending;

    rsx! {
        div { class: "flexible-page",
            // 顶部 header
            PageHeader {
                title: "灵活模式".to_string(),
                class: Some("dx-page-header--nested".to_string()),
                actions: Some(rsx! {
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::IconSm,
                        disabled: busy,
                        title: "清空会话",
                        onclick: move |_| {
                            if let Some(ref svc) = *chat_signal.read() {
                                svc.stop();
                                if let Err(e) = svc.reset_session() {
                                    tracing::error!("清空会话重置失败: {}", e);
                                }
                            }
                            chat.clear();
                        },
                        Trash2 { size: "16" }
                    }
                }),
            }

            // 通用聊天面板
            ChatPanel {
                chat,
                chat_signal,
                on_user_action: move |(action, choice, pending)| {
                    handle_user_action(action, choice, pending, chat, chat_signal);
                },
                template_label: crate::components::chat::chat_panel::template_label(&current_template),
                templates: templates.read().clone(),
                on_template_change: Some(Callback::new(move |name: String| {
                    if name.is_empty() {
                        return;
                    }
                    if let Some(ref svc) = *chat_signal.read() {
                        svc.stop();
                        svc.set_system_prompt_template(Some(name.clone()));
                        if let Err(e) = svc.reset_session() {
                            tracing::error!("重置会话失败: {}", e);
                        }
                    }
                    chat.clear_pending();
                    chat.pending_tool_call_id.set(None);
                    template.set(Some(name));
                })),
                thinking: *thinking.read(),
                on_thinking_change: Some(Callback::new(move |v: bool| thinking.set(v))),
                temperature: temperature.read().clone(),
                on_temperature_change: Some(Callback::new(move |v: String| temperature.set(v))),
                on_clear: Callback::new(move |_| {
                    if let Some(ref svc) = *chat_signal.read() {
                        svc.stop();
                        if let Err(e) = svc.reset_session() {
                            tracing::error!("清空会话重置失败: {}", e);
                        }
                    }
                    chat.clear();
                }),
            }
        }
    }
}
