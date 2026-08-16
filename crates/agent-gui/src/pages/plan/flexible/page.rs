//! 灵活模式主组件：基于 v2_chat 的聊天面板（含子 agent 支持）。
//!
//! 使用通用 `ChatPanel` 组件渲染 UI，自身只负责：
//! - V2ChatService 初始化与事件订阅
//! - 子 agent 注册（demo_agent）
//! - 模板切换与可用模板列表

use std::sync::{Arc, OnceLock};

use dioxus::prelude::*;
use planned_agent::v2_chat::{V2ChatConfig, V2ChatSubAgentRunner};
use planned_agent::V2ChatService;
use planned_agent_core::prompt::PromptManager;
use planned_agent_tool_manager::ToolCategory;

use crate::components::chat::{ChatPanel, chat_flow::{ChatSignals, ensure_subscription, handle_user_action, PendingUI}};
use crate::components::page_header::PageHeader;
use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::context::{AiContext, PromptContext, ToolsContext};
use crate::services::chat_service::{use_v2_chat_service, V2ChatServiceSignal};

use dioxus_icons::lucide::Trash2;

/// 子 agent 注册幂等锁（进程内只注册一次）
static SUB_AGENT_REGISTERED: OnceLock<()> = OnceLock::new();

/// 子 agent 工具名称
const SUB_AGENT_TOOL_NAME: &str = "demo_agent";

/// 子 agent 使用的 prompt 模板
const SUB_AGENT_PROMPT: &str = "chat/request_user_action_demo";

/// 默认父 agent 的 system prompt 模板
const DEFAULT_TEMPLATE: &str = "thorough/thorough_system";

#[component]
pub fn FlexiblePage() -> Element {
    // ── 纯内存聊天状态 ──
    let messages = use_signal_sync(|| Vec::<planned_agent_core::ai::types::Message>::new());
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

    // ── option 栏状态 ──
    let mut thinking = use_signal_sync(|| true);
    let mut temperature = use_signal_sync(|| "0.7".to_string());

    // ── 父 agent V2ChatService ──
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
                let _ = svc.start_driver();
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

            let runner = V2ChatSubAgentRunner::new(child_service, 0, 3);

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
                        onclick: move |_| chat.clear(),
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
                on_clear: Callback::new(move |_| chat.clear()),
            }
        }
    }
}
