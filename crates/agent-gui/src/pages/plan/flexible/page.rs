//! 灵活模式主组件：基于 chat 的聊天面板（含子 agent 支持）。
//!
//! 使用通用 `ChatPanel` 组件渲染 UI，自身只负责：
//! - ChatService 初始化与事件订阅
//! - 模板切换与可用模板列表
//! - 历史加载

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::chat::ChatConfig;
use planned_agent::ChatService;
use planned_agent_core::prompt::PromptManager;
use planned_agent_prompt_manager::FilePromptManager;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::{
    chat_flow::{ensure_subscription, handle_user_action, ChatMessage, ChatSignals, PendingUI},
    ChatPanel,
};
use crate::components::page_header::PageHeader;
use crate::context::{
    require_resource, storage_repo, AiContext, PromptContext, StorageContext, ToolsContext,
};
use crate::storage::ChatMessageStore;

use dioxus_icons::lucide::Trash2;

#[derive(Props, Clone, PartialEq)]
pub struct FlexiblePageProps {
    pub plan_id: String,
}

#[component]
pub fn FlexiblePage(props: FlexiblePageProps) -> Element {
    let plan_id = props.plan_id.clone();

    // ── 纯内存聊天状态（展示缓冲，持久化由服务端 store 处理）──
    let messages = use_signal_sync(Vec::<ChatMessage>::new);
    let pending_ui = use_signal_sync(|| None::<PendingUI>);
    let input_text = use_signal_sync(String::new);
    let pending_tool_call_id = use_signal_sync(|| None::<String>);
    let subscription = use_signal_sync(|| None::<planned_agent::chat::SubscriptionGuard>);
    let mut chat = ChatSignals {
        messages,
        pending_ui,
        input_text,
        pending_tool_call_id,
        subscription,
    };

    // ── option 栏状态 ──
    let mut thinking = use_signal_sync(|| true);
    let mut temperature = use_signal_sync(|| "0.7".to_string());
    let mut template = use_signal_sync(|| Some("flexible/flexible_global_system".to_string()));

    // ── ChatService：Signal 存储，首次构造后跨 re-render 存活 ──
    // ⚠️ 所有 hook 无条件顶层调用（rules of hooks），只有纯构造逻辑在 if 内
    let mut svc_opt = use_signal_sync(|| None::<Arc<ChatService<FilePromptManager>>>);
    let storage_resource = use_context::<Resource<Option<Arc<StorageContext>>>>();
    let ai_ctx = require_resource::<AiContext>();
    let tools_ctx = require_resource::<ToolsContext>();
    let prompt_ctx = require_resource::<PromptContext>();

    if svc_opt.read().is_none() {
        let svc = Arc::new({
            if let Some(repo) = storage_repo(storage_resource, |ctx| ctx.chat_message_repo()) {
                let store = ChatMessageStore::new(plan_id.clone(), repo);
                ChatService::with_store(
                    ai_ctx.manager.default().expect("获取默认 AI client 失败"),
                    tools_ctx.registry.clone(),
                    prompt_ctx.manager.clone(),
                    ChatConfig {
                        allowed_tools: None,
                        ..Default::default()
                    },
                    Arc::new(store),
                )
            } else {
                ChatService::new(
                    (*ai_ctx.manager).clone(),
                    tools_ctx.registry.clone(),
                    prompt_ctx.manager.clone(),
                    ChatConfig {
                        allowed_tools: None,
                        ..Default::default()
                    },
                )
                .expect("ChatService 构造失败")
            }
        });
        svc.start_driver().expect("ChatService driver 启动失败");
        svc_opt.set(Some(svc));
    }

    // 从 Signal 取出服务引用（后续 re-render 读缓存，不重建）
    let chat_service = svc_opt.read().clone().unwrap();

    // ── 事件订阅 + 历史加载：service ready 后执行一次 ──
    {
        let mut initialized = use_signal_sync(|| false);
        use_effect(move || {
            if *initialized.read() || svc_opt.read().is_none() {
                return;
            }
            initialized.set(true);
            let svc = svc_opt.read().clone().unwrap();
            // 从服务端 store 恢复历史
            let history = svc.history();
            if !history.is_empty() {
                tracing::info!("灵活模式: 从服务端加载 {} 条历史消息", history.len());
                chat.load_from_history(&history);
            }
            // 注册事件订阅（guard 存入 chat.subscription signal，跨 re-render 存活）
            ensure_subscription(&mut chat, &svc);
        });
    }

    // ── 可用模板列表 ──
    let mut templates = use_signal_sync(|| Vec::<String>::new());
    let prompt_resource = use_context::<Resource<Option<Arc<PromptContext>>>>();
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

    // ── 事件处理函数 ──

    let on_trash_click = {
        let svc = chat_service.clone();
        move |_| {
            svc.stop();
            if let Err(e) = svc.reset_session() {
                tracing::error!("清空会话重置失败: {}", e);
            }
            chat.clear();
        }
    };

    let on_user_action = {
        let svc = chat_service.clone();
        move |(action, choice, pending)| {
            handle_user_action(&mut chat, &svc, action, choice, pending);
        }
    };

    let on_template_change = {
        let svc = chat_service.clone();
        Callback::new(move |name: String| {
            if name.is_empty() {
                return;
            }
            svc.stop();
            svc.set_system_prompt_template(Some(name.clone()));
            if let Err(e) = svc.reset_session() {
                tracing::error!("重置会话失败: {}", e);
            }
            chat.clear_pending();
            chat.pending_tool_call_id.set(None);
            template.set(Some(name));
        })
    };

    let on_thinking_change = Callback::new(move |v: bool| thinking.set(v));
    let on_temperature_change = Callback::new(move |v: String| temperature.set(v));

    let on_clear = {
        let svc = chat_service.clone();
        Callback::new(move |_| {
            svc.stop();
            if let Err(e) = svc.reset_session() {
                tracing::error!("清空会话重置失败: {}", e);
            }
            chat.clear();
        })
    };

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
                        onclick: on_trash_click,
                        Trash2 { size: "16" }
                    }
                }
                },
            }

            ChatPanel {
                chat,
                chat_service: chat_service.clone(),
                on_user_action,
                template_label: crate::components::chat::chat_panel::template_label(&current_template),
                templates: templates.read().clone(),
                on_template_change: Some(on_template_change),
                thinking: *thinking.read(),
                on_thinking_change: Some(on_thinking_change),
                temperature: temperature.read().clone(),
                on_temperature_change: Some(on_temperature_change),
                on_clear,
            }
        }
    }
}
