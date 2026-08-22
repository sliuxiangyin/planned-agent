//! 灵活模式主组件：基于 chat 的聊天面板（含子 agent 支持）。
//!
//! 使用通用 `ChatPanel` 组件渲染 UI，自身只负责：
//! - ChatService 初始化与事件订阅
//! - 子 agent 注册（demo_agent）
//! - 模板切换与可用模板列表
//! - 聊天消息持久化（RoundEnd 时写入 DB）+ 历史加载

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::chat::ChatConfig;
use planned_agent_core::prompt::PromptManager;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::{
    chat_flow::{ChatMessage, ensure_subscription, handle_user_action, ChatSignals, PendingUI},
    ChatPanel,
};
use crate::components::page_header::PageHeader;
use crate::context::{storage_repo, PromptContext, StorageContext};
use crate::pages::plan::shared::load_chat_messages::load_chat_messages;
use crate::services::chat_service::{use_chat_service, use_sub_agent, ChatServiceSignal};
use crate::storage::repository::ChatMessageRepo;

use dioxus_icons::lucide::Trash2;

#[derive(Props, Clone, PartialEq)]
pub struct FlexiblePageProps {
    pub plan_id: String,
}

#[component]
pub fn FlexiblePage(props: FlexiblePageProps) -> Element {
    let plan_id = props.plan_id.clone();

    // ── 纯内存聊天状态（ChatMessage 自包含 reasoning/streaming/tool_calls）──
    let messages = use_signal_sync(Vec::<ChatMessage>::new);
    let pending_ui = use_signal_sync(|| None::<PendingUI>);
    let input_text = use_signal_sync(String::new);
    let pending_tool_call_id = use_signal_sync(|| None::<String>);
    let subscription = use_signal_sync(|| None::<planned_agent::chat::SubscriptionGuard>);
    let persist_signal = use_signal_sync(|| None::<Arc<dyn Fn(&ChatMessage) + Send + Sync>>);
    let mut chat = ChatSignals {
        messages,
        pending_ui,
        input_text,
        pending_tool_call_id,
        subscription,
        persist: persist_signal,
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
    let chat_signal: ChatServiceSignal = use_chat_service(initial_config);

    // ── Storage 上下文（提取一次，多处复用） ──
    let storage_resource = use_context::<Resource<Option<Arc<StorageContext>>>>();

    // ── 加载历史消息（仅首次） ──
    {
        let pid = plan_id.clone();
        use_effect(move || {
            tracing::debug!(target: "chat_flow", pid = %pid, "load_history use_effect 执行");
            if let Some(repo) = storage_repo(storage_resource, |ctx| ctx.chat_message_repo()) {
                tracing::debug!(target: "chat_flow", pid = %pid, "load_history: repo 获取成功，开始加载");
                let pid = pid.clone();
                let msgs = messages;
                spawn(load_history(pid, repo, msgs));
            } else {
                tracing::debug!(target: "chat_flow", pid = %pid, "load_history: storage_repo 返回 None，跳过");
            }
        });
    }

    // ── 持久化回调（Service ready 后设置） ──
    {
        let pid = plan_id.clone();
        let persist_signal = chat.persist;
        use_effect(move || {
            tracing::debug!(target: "persist", pid = %pid, "persist use_effect 执行");
            if let Some(repo) = storage_repo(storage_resource, |ctx| ctx.chat_message_repo()) {
                tracing::debug!(target: "persist", pid = %pid, "persist: repo 获取成功");
                let pid = pid.clone();
                let mut ps = persist_signal;
                ps.set(Some(build_persist_fn(pid, repo)));
            } else {
                tracing::debug!(target: "persist", pid = %pid, "persist: storage_repo 返回 None，跳过");
            }
        });
    }

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

/// 根据 ChatMessage 内容确定 msg_type。
fn determine_msg_type(cm: &ChatMessage) -> &'static str {
    use planned_agent_core::ai::types::MessageRole;

    match cm.message.role {
        MessageRole::User => "user",
        MessageRole::Assistant => {
            if cm.message.tool_calls.is_some() && !cm.message.tool_calls.as_ref().unwrap().is_empty()
            {
                "tool_call"
            } else if cm.message.reasoning_content.is_some() {
                "reasoning"
            } else {
                "text"
            }
        }
        _ => "text",
    }
}

/// 加载历史消息并写入 Signal。
async fn load_history(
    pid: String,
    repo: Arc<ChatMessageRepo>,
    mut msgs: Signal<Vec<ChatMessage>, SyncStorage>,
) {
    tracing::debug!(target: "chat_flow", pid = %pid, "load_history: 开始加载");
    let loaded = load_chat_messages(&pid, repo).await;
    tracing::debug!(target: "chat_flow", count = loaded.len(), "load_history: 加载完成");
    if !loaded.is_empty() {
        tracing::info!("灵活模式: 加载 {} 条历史消息", loaded.len());
        msgs.set(loaded);
    }
}

/// 构建持久化回调：RoundEnd 时将 ChatMessage 写入 DB。
fn build_persist_fn(
    pid: String,
    repo: Arc<ChatMessageRepo>,
) -> Arc<dyn Fn(&ChatMessage) + Send + Sync> {
    Arc::new(move |cm: &ChatMessage| {
        tracing::debug!(
            target: "persist",
            sequence_order = cm.sequence_order,
            role = ?cm.message.role,
            is_streaming = cm.is_streaming,
            tool_calls_count = cm.tool_call_entries.len(),
            "persist_streaming 被调用"
        );
        let repo = repo.clone();
        let pid = pid.clone();
        let cm = cm.clone();
        tokio::spawn(async move {
            let msg_type = determine_msg_type(&cm);
            let msg_json = match serde_json::to_string(&cm.message) {
                Ok(j) => j,
                Err(e) => {
                    tracing::error!("序列化消息失败: {}", e);
                    return;
                }
            };
            if let Err(e) = repo
                .create(&pid, &msg_json, cm.sequence_order as i32, &msg_type, false)
                .await
            {
                tracing::error!("持久化聊天消息失败: {}", e);
            }
        });
    })
}
