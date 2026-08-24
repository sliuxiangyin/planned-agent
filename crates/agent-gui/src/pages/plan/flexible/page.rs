//! 灵活模式主组件：基于 chat 的聊天面板（含子 agent 支持）。
//!
//! 使用通用 `ChatPanel` 组件渲染 UI，自身只负责：
//! - ChatService 初始化与事件订阅
//! - 模板切换与可用模板列表
//! - 聊天消息持久化（RoundEnd 时写入 DB）+ 历史加载

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::chat::ChatConfig;
use planned_agent_core::prompt::PromptManager;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::{
    chat_flow::{
        ensure_subscription, handle_user_action, ChatContext, ChatMessage, ChatSignals,
        ChatStorage, DummyStorage, PendingUI,
    },
    ChatPanel,
};
use crate::components::page_header::PageHeader;
use crate::context::{storage_repo, PromptContext, StorageContext};
use crate::services::chat_service::{use_chat_service, ChatServiceSignal};
use crate::storage::ChatMessageStorage;

use dioxus_icons::lucide::Trash2;

#[derive(Props, Clone, PartialEq)]
pub struct FlexiblePageProps {
    pub plan_id: String,
}

#[component]
pub fn FlexiblePage(props: FlexiblePageProps) -> Element {
    let plan_id = props.plan_id.clone();

    // ── 会话上下文（storage + plan_id），初始化后只读 ──
    let ctx_sig = use_signal_sync(|| ChatContext {
        storage: Arc::new(DummyStorage),
        plan_id: plan_id.clone(),
    });

    // ── 纯内存聊天状态 ──
    let messages = use_signal_sync(Vec::<ChatMessage>::new);
    let pending_ui = use_signal_sync(|| None::<PendingUI>);
    let input_text = use_signal_sync(String::new);
    let pending_tool_call_id = use_signal_sync(|| None::<String>);
    let subscription = use_signal_sync(|| None::<planned_agent::chat::SubscriptionGuard>);
    let last_persisted_seq = use_signal_sync(|| 0u64);
    let mut chat = ChatSignals {
        messages,
        pending_ui,
        input_text,
        pending_tool_call_id,
        subscription,
        last_persisted_seq,
        ctx: ctx_sig,
    };

    // ── option 栏状态 ──
    let mut thinking = use_signal_sync(|| true);
    let mut temperature = use_signal_sync(|| "0.7".to_string());

    // ── 父 agent ChatService ──
    let mut template = use_signal_sync(|| Some("flexible/flexible_global_system".to_string()));
    let initial_config = ChatConfig {
        allowed_tools: None,
        ..Default::default()
    };
    let chat_service_signal: ChatServiceSignal = use_chat_service(initial_config);

    // ── Storage 上下文 ──
    let storage_resource = use_context::<Resource<Option<Arc<StorageContext>>>>();

    // ── 初始化 ChatStorage + ChatContext，并加载历史消息（仅首次） ──
    {
        let pid = plan_id.clone();
        let mut msgs = messages;
        let mut ctx_s = ctx_sig;
        let mut last_persisted = last_persisted_seq;
        use_effect(move || {
            if let Some(repo) = storage_repo(storage_resource, |ctx| ctx.chat_message_repo()) {                let storage: Arc<dyn ChatStorage> = Arc::new(ChatMessageStorage::new(repo));
                ctx_s.set(ChatContext {
                    storage: storage.clone(),
                    plan_id: pid.clone(),
                });
                let pid = pid.clone();
                spawn(async move {
                    tracing::debug!(target: "chat_flow", pid = %pid, "load_history: 开始加载");
                    let loaded = storage.load_messages(&pid).await;
                    tracing::debug!(target: "chat_flow", count = loaded.len(), "load_history: 加载完成");
                    if !loaded.is_empty() && msgs.read().is_empty() {
                        tracing::info!("灵活模式: 加载 {} 条历史消息", loaded.len());
                        // 历史消息已持久化，游标推进到最大 seq，
                        // 避免后续 RoundEnd 把历史消息当作新增重写
                        let max_seq = loaded.iter().map(|m| m.sequence_order).max().unwrap_or(0);
                        last_persisted.set(max_seq);
                        msgs.set(loaded);
                    } else if !loaded.is_empty() {
                        // 加载期间用户已发起新消息：不覆盖内存，避免新消息与游标错位丢失
                        tracing::info!("灵活模式: 加载期间已有新消息，跳过历史覆盖");
                    }
                });
            }
        });
    }

    // ── 事件订阅：service ready 后注册一次 ──
    {
        let svc_sig: ChatServiceSignal = chat_service_signal;
        let mut chat_for_sub = chat;
        use_effect(move || {
            if let Some(ref svc) = *svc_sig.read() {
                let _ = svc.start_driver();
                ensure_subscription(&mut chat_for_sub, svc);
            }
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

    let current_template = template.read().clone().unwrap_or_default();
    let is_streaming = chat.is_streaming();
    let has_pending = chat.has_pending();
    let busy = is_streaming || has_pending;

    rsx! {
        div { class: "flexible-page",
            PageHeader {
                title: "灵活模式".to_string(),
                class: Some("dx-page-header--nested".to_string()),
                actions: {
                    rsx! {
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::IconSm,
                        disabled: busy,
                        title: "清空会话",
                        onclick: move |_| {
                            if let Some(ref svc) = *chat_service_signal.read() {
                                svc.stop();
                                if let Err(e) = svc.reset_session() {
                                    tracing::error!("清空会话重置失败: {}", e);
                                }
                            }
                            chat.clear();
                            let ctx = chat.ctx.read();
                            let storage = ctx.storage.clone();
                            let pid = ctx.plan_id.clone();
                            tokio::spawn(async move { storage.delete_messages(&pid).await; });
                        },
                        Trash2 { size: "16" }
                    }
                }
                },
            }

            ChatPanel {
                chat,
                chat_service_signal,
                on_user_action: move |(action, choice, pending)| {
                    handle_user_action(&mut chat, chat_service_signal, action, choice, pending);
                },
                template_label: crate::components::chat::chat_panel::template_label(&current_template),
                templates: templates.read().clone(),
                on_template_change: Some(Callback::new(move |name: String| {
                    if name.is_empty() { return; }
                    if let Some(ref svc) = *chat_service_signal.read() {
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
                on_clear: {
                    Callback::new(move |_| {
                        if let Some(ref svc) = *chat_service_signal.read() {
                            svc.stop();
                            if let Err(e) = svc.reset_session() {
                                tracing::error!("清空会话重置失败: {}", e);
                            }
                        }
                        chat.clear();
                        let ctx = chat.ctx.read();
                        let storage = ctx.storage.clone();
                        let pid = ctx.plan_id.clone();
                        tokio::spawn(async move { storage.delete_messages(&pid).await; });
                    })
                },
            }
        }
    }
}
