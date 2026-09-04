//! 灵活模式页面控制器 —— 收口页面所有 signal / 异步初始化 / 事件逻辑。
//!
//! 把原本散落在 `page.rs` 组件体里的：
//! - 聊天/选项栏 signal
//! - ChatService 异步初始化（storage ready → ensure_current_session → 绑会话 store）
//! - 历史加载 + 事件订阅
//! - 可用模板列表加载
//! - 子 agent 注册 / 注销
//! - 事件处理方法（清空、模板切换、用户操作）
//!
//! 全部收进 `use_flexible_controller` 这一个自定义 hook 返回的 `FlexibleController`，
//! 让 `FlexiblePage` 组件退化为「状态 → 视图」的纯渲染层。
//!
//! 设计要点：
//! - `ChatSignals` 是 `Copy`（内部全为 `Signal`），controller 可直接持有副本；
//!   需要 `&mut ChatSignals` 的方法用 `let mut chat = self.chat;` 拷贝一份再调用（状态经信号共享）。
//! - 所有 `use_*` 在该 hook 内无条件、顺序稳定调用（遵守 rules of hooks）。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::anyhow;
use dioxus::prelude::*;
use planned_agent::chat::{ChatConfig, SubscriptionGuard};
use planned_agent::ChatService;
use planned_agent_core::events::UIAction;
use planned_agent_core::prompt::PromptManager;
use planned_agent_prompt_manager::FilePromptManager;

use crate::components::chat::chat_flow::{
    ensure_subscription, handle_user_action, Bubble, ChatSignals, PendingUI,
};
use crate::context::{
    register_sub_agent, require_resource, storage_repo, AiContext, PromptContext, StorageContext,
    ToolsContext,
};
use crate::services::plans_flexible_service::PlansFlexibleService;

use super::chat_flexible_message_storage::ChatMessageStore;
use super::step2_callback::create_step2_callback;
use super::step5_callback::create_step5_callback;

/// 便捷类型：灵活模式所用 ChatService。
pub(crate) type ChatSvc = ChatService<FilePromptManager>;

/// ChatService 创建工厂：固化"造一个绑某 session store 的 ChatService"所需的依赖，
/// 供初始化与 `switch_session` 复用。依赖均为 Arc，可 Clone、可反复调用。
#[derive(Clone)]
pub(crate) struct ChatServiceFactory {
    storage: Arc<StorageContext>,
    plan_id: String,
    ai: Arc<AiContext>,
    tools: Arc<ToolsContext>,
    prompt: Arc<PromptContext>,
}

impl ChatServiceFactory {
    fn new(
        storage: Arc<StorageContext>,
        plan_id: String,
        ai: Arc<AiContext>,
        tools: Arc<ToolsContext>,
        prompt: Arc<PromptContext>,
    ) -> Self {
        Self {
            storage,
            plan_id,
            ai,
            tools,
            prompt,
        }
    }

    /// 为指定 session 造一个绑定该 session store 的 ChatService（未 start_driver）。
    pub(crate) fn build_for_session(&self, session_id: &str) -> anyhow::Result<ChatSvc> {
        let repo = self.storage.chat_message_repo();
        let store = ChatMessageStore::new(self.plan_id.clone(), session_id.to_string(), repo);
        Ok(ChatService::with_store(
            self.ai.manager.default()?,
            self.tools.registry.clone(),
            self.prompt.manager.clone(),
            ChatConfig {
                system_prompt_template: Some("flexible/flexible_global_system".to_string()),
                allowed_tools: None,
                ..Default::default()
            },
            Arc::new(store),
        ))
    }
}

/// 灵活模式控制器：持有全部状态 signal 与 ChatService，并提供事件处理方法。
#[derive(Clone, Copy)]
pub(crate) struct FlexibleController {
    /// 纯内存聊天状态（展示缓冲；持久化由服务端 store 负责）。
    pub chat: ChatSignals,
    /// 是否启用思考模式
    thinking: Signal<bool, SyncStorage>,
    /// 温度值
    temperature: Signal<String, SyncStorage>,
    /// 当前系统提示模板
    template: Signal<Option<String>, SyncStorage>,
    /// 可用模板列表
    templates: Signal<Vec<String>, SyncStorage>,
    /// ChatService（初始化完成后 Some）
    svc: Signal<Option<Arc<ChatSvc>>, SyncStorage>,
    /// ChatService 创建工厂（storage 就绪后由初始化填充，供 switch_session 复用）
    #[allow(dead_code)] // 待"历史翻回"UI 接线 switch_session 后读取
    factory: Signal<Option<ChatServiceFactory>, SyncStorage>,
    /// 当前活跃会话 id（service 就绪后必有值；UI 响应式展示来源）
    session_id: Signal<String, SyncStorage>,
    /// 共享"当前会话"槽（供 step5 callback 等 `'static` 旁路读取；随 session_id 同步）
    session_slot: Signal<Arc<RwLock<Option<String>>>, SyncStorage>,
    /// 初始化是否已尝试（防止重复 spawn）
    #[allow(dead_code)] // 由 hook 内闭包写入，跨 render 保持
    svc_started: Signal<bool, SyncStorage>,
    /// 历史/订阅是否已执行过一次
    #[allow(dead_code)] // 由 hook 内闭包写入，跨 render 保持
    initialized: Signal<bool, SyncStorage>,
}

impl FlexibleController {
    /// 就绪后的 ChatService；初始化完成前为 None。
    pub(crate) fn service(&self) -> Option<Arc<ChatSvc>> {
        self.svc.read().clone()
    }

    // ── 只读访问器 ────────────────────────────────────────────────
    pub(crate) fn is_busy(&self) -> bool {
        self.chat.is_busy()
    }
    pub(crate) fn thinking(&self) -> bool {
        *self.thinking.read()
    }
    pub(crate) fn temperature(&self) -> String {
        self.temperature.read().clone()
    }
    pub(crate) fn template(&self) -> String {
        self.template.read().clone().unwrap_or_default()
    }
    /// 当前活跃会话 id（service 就绪前为空串）。
    #[allow(dead_code)] // 预留：供 UI 展示 / 历史翻回等读取
    pub(crate) fn session_id(&self) -> String {
        self.session_id.read().clone()
    }
    pub(crate) fn templates(&self) -> Vec<String> {
        self.templates.read().clone()
    }

    // ── 事件处理方法 ─────────────────────────────────────────────
    /// 清空会话（停止 + 重置服务端会话 + 清空气泡）。
    pub(crate) fn clear_session(&self) {
        if let Some(svc) = self.service() {
            svc.stop();
            if let Err(e) = svc.reset_session() {
                tracing::error!("清空会话重置失败: {}", e);
            }
        }
        let mut chat = self.chat;
        chat.clear();
    }

    /// 用户点击 request_user_action / 子 agent 卡片后的回调。
    pub(crate) fn on_user_action(&self, action: UIAction, choice: String, pending: PendingUI) {
        let Some(svc) = self.service() else { return };
        let mut chat = self.chat;
        handle_user_action(&mut chat, &svc, action, choice, pending);
    }

    /// 切换到指定 session：停掉旧对话并退订，为它新建一个绑该 session store 的
    /// ChatService，并复位 `initialized` 使下方的历史/订阅 effect 对新 service 重跑
    /// （自动加载该 session 历史现场）。供外部（如历史版本翻回）直接调用。
    #[allow(dead_code)] // 预留 API：待"历史翻回"UI 接线后使用
    pub(crate) fn switch_session(&self, session_id: String) -> anyhow::Result<()> {
        let Some(factory) = self.factory.read().clone() else {
            return Err(anyhow!("ChatServiceFactory 尚未就绪，无法切换会话"));
        };
        // 停掉旧对话并退订旧订阅（guard 一 drop 即自动退订）
        if let Some(old) = self.service() {
            old.stop();
        }
        let mut chat = self.chat;
        chat.subscription.set(None);
        chat.clear();

        // 为目标 session 构造绑该 store 的新 ChatService
        let service = factory.build_for_session(&session_id)?;

        // 在与应用初始化一致的运行时（dioxus spawn）里启动 driver 并挂载；
        // svc 变化会触发 history/subscription effect 对新 service 重跑。
        let slot = self.session_slot.read().clone();
        let mut session_id_sig = self.session_id;
        let mut svc = self.svc;
        let mut initialized = self.initialized;
        let target_session = session_id.clone();
        spawn(async move {
            if let Err(e) = service.start_driver() {
                tracing::error!("灵活模式切换会话: ChatService driver 启动失败: {}", e);
                return;
            }
            // 记录当前会话：UI 信号 + step5 共享槽
            if let Ok(mut g) = slot.write() {
                *g = Some(target_session.clone());
            }
            session_id_sig.set(target_session);
            initialized.set(false);
            svc.set(Some(Arc::new(service)));
        });
        Ok(())
    }

    /// 切换系统提示模板（切换即停当前会话并重置）。
    pub(crate) fn apply_template(&self, name: String) {
        if name.is_empty() {
            return;
        }
        if let Some(svc) = self.service() {
            svc.stop();
            svc.set_system_prompt_template(Some(name.clone()));
            if let Err(e) = svc.reset_session() {
                tracing::error!("重置会话失败: {}", e);
            }
        }
        let mut chat = self.chat;
        chat.clear_pending();
        chat.pending_tool_call_id.set(None);
        let mut template = self.template;
        template.set(Some(name));
    }

    pub(crate) fn set_thinking(&self, v: bool) {
        let mut thinking = self.thinking;
        thinking.set(v);
    }
    pub(crate) fn set_temperature(&self, v: String) {
        let mut temperature = self.temperature;
        temperature.set(v);
    }
}

/// 创建/复用 flexible 页面控制器。组件须在顶层无条件调用。
pub(crate) fn use_flexible_controller(plan_id: String) -> FlexibleController {
    // ── 纯内存聊天状态 ──
    let mut chat = ChatSignals {
        bubbles: use_signal_sync(Vec::<Bubble>::new),
        active: use_signal_sync(Vec::<Bubble>::new),
        agent_views: use_signal_sync(|| HashMap::new()),
        pending_ui: use_signal_sync(|| None::<PendingUI>),
        input_text: use_signal_sync(String::new),
        pending_tool_call_id: use_signal_sync(|| None::<String>),
        subscription: use_signal_sync(|| None::<SubscriptionGuard>),
    };

    // ── option 栏状态 ──
    let thinking = use_signal_sync(|| true);
    let temperature = use_signal_sync(|| "0.7".to_string());
    let template = use_signal_sync(|| Some("flexible/flexible_step1".to_string()));

    // ── ChatService：signal 存储；异步初始化（storage ready → ensure session → 建绑会话 store）──
    let mut svc = use_signal_sync(|| None::<Arc<ChatSvc>>);
    let mut factory = use_signal_sync(|| None::<ChatServiceFactory>);
    let mut svc_started = use_signal_sync(|| false);
    let mut initialized = use_signal_sync(|| false);
    let mut templates = use_signal_sync(|| Vec::<String>::new());
    // 当前活跃会话 id（service 就绪后必有值）与其共享槽（供 step5 callback 读取）
    let session_id = use_signal_sync(|| String::new());
    let session_slot = use_signal_sync(|| Arc::new(RwLock::new(None::<String>)));

    // ── 依赖 context ──
    let storage_resource = use_context::<Resource<Option<Arc<StorageContext>>>>();
    let ai_ctx = require_resource::<AiContext>();
    let tools_ctx = require_resource::<ToolsContext>();
    let prompt_ctx = require_resource::<PromptContext>();

    // ── 供异步初始化闭包捕获的 owned clone ──
    let plan_id_c = plan_id.clone();
    let ai_ctx_c = ai_ctx.clone();
    let tools_ctx_c = tools_ctx.clone();
    let prompt_ctx_c = prompt_ctx.clone();
    let storage_resource_c = storage_resource.clone();

    // ── ChatService 异步初始化 ──
    use_effect(move || {
        if svc.read().is_some() || *svc_started.read() {
            return;
        }
        let storage = storage_resource_c
            .read()
            .as_ref()
            .and_then(|x| x.as_ref())
            .cloned();
        let Some(storage) = storage else {
            return; // storage 尚未就绪，等下次 re-render 再尝试
        };
        svc_started.set(true);

        let plan_id = plan_id_c.clone();
        let ai_ctx = ai_ctx_c.clone();
        let tools_ctx = tools_ctx_c.clone();
        let prompt_ctx = prompt_ctx_c.clone();
        let slot = session_slot.read().clone();
        let mut session_id_sig = session_id;
        spawn(async move {
            let built = async {
                let session = storage.ensure_current_session(&plan_id).await?;
                let session_id = session.id.clone();
                let factory_obj = ChatServiceFactory::new(
                    storage.clone(),
                    plan_id.clone(),
                    ai_ctx.clone(),
                    tools_ctx.clone(),
                    prompt_ctx.clone(),
                );
                let service = factory_obj.build_for_session(&session.id)?;
                Ok::<(String, ChatServiceFactory, ChatSvc), anyhow::Error>((
                    session_id,
                    factory_obj,
                    service,
                ))
            }
            .await;

            match built {
                Ok((session_id, factory_obj, service)) => {
                    if let Err(e) = service.start_driver() {
                        tracing::error!("ChatService driver 启动失败: {}", e);
                        return;
                    }
                    // 记录当前会话：共享槽（step5 callback 读取）+
                    // UI 信号（service 就绪后必有值）
                    if let Ok(mut g) = slot.write() {
                        *g = Some(session_id.clone());
                    }
                    session_id_sig.set(session_id);
                    factory.set(Some(factory_obj));
                    svc.set(Some(Arc::new(service)));
                }
                Err(e) => tracing::error!("灵活模式 ChatService 初始化失败: {}", e),
            }
        });
    });

    // ── 事件订阅 + 历史加载：service ready 后执行一次 ──
    use_effect(move || {
        if *initialized.read() || svc.read().is_none() {
            return;
        }
        initialized.set(true);
        let Some(service) = svc.read().clone() else { return };
        // 从服务端 store 恢复历史
        let history = service.history_store();
        if !history.is_empty() {
            tracing::info!("灵活模式: 从服务端加载 {} 条历史消息", history.len());
            chat.load_from_history(&history);
        }
        // 注册事件订阅（guard 存入 chat.subscription signal，跨 re-render 存活）
        ensure_subscription(&mut chat, &service);
    });

    // ── 可用模板列表 ──
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

    // ── 注册子 agent（生命周期方案，避免重复注册与孤儿工具）──
    let registry = tools_ctx.registry.clone();
    use_hook(move || {
        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_step1",
            "需求澄清子 Agent：将用户自然语言需求澄清为可执行的任务定义。接收用户消息和历史对话摘要，根据预设规则进行需求分析和参数提取。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "user_message": {
                        "type": "string",
                        "description": "用户本次输入内容"
                    },
                    "conversation_summary": {
                        "type": "string",
                        "description": "历史对话摘要（若无则传空字符串）"
                    }
                },
                "required": ["user_message"]
            }),
            ChatConfig {
                system_prompt_template: Some("flexible/flexible_step1".into()),
                allowed_tools: Some(vec!["request_user_action".to_string()]),
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            None,
        );
        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_step2",
            "灵活模式任务执行 Agent：根据需求澄清结果执行工具调用并记录轨迹。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task_definition": {
                        "type": "string",
                        "description": "来自 flexible_step1 的 Markdown 任务定义，包含任务描述和参数"
                    },
                    "runtime_context": {
                        "type": "string",
                        "description": "可选，来自上一轮执行的 compressed_context；首次执行时为空"
                    }
                },
                "required": ["task_definition"]
            }),
            ChatConfig {
                system_prompt_template: Some("flexible/flexible_step2".into()),
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            create_step2_callback(),
        );
        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_step3",
            "灵活模式字段选择 Agent：从 step2 执行结果中提取可用字段，通过交互让用户选择最终输出的字段。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "execution_trace_summary": {
                        "type": "string",
                        "description": "来自 flexible_step2 的执行轨迹摘要（compressed_context），包含工具调用记录和输出数据"
                    },
                    "output_format": {
                        "type": "string",
                        "description": "来自 flexible_step1 的输出格式，已由用户确认（如 Excel、CSV、JSON、文本等）"
                    }
                },
                "required": ["execution_trace_summary"]
            }),
            ChatConfig {
                system_prompt_template: Some("flexible/flexible_step3".into()),
                allowed_tools: Some(vec!["request_user_action".to_string()]),
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            None,
        );
        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_step4",
            "灵活模式参数确认 Agent：分析 step2 执行轨迹中的具体参数值，识别可参数化候选，与用户确认后生成最终的模板输入定义。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "execution_trace": {
                        "type": "string",
                        "description": "来自 flexible_step2 的完整执行轨迹，包含每次工具调用的输入参数"
                    },
                    "output_format": {
                        "type": "string",
                        "description": "来自 flexible_step1 的输出格式，已由用户确认（如 Excel、CSV、JSON、文本等）"
                    },
                    "field_selection_result": {
                        "type": "string",
                        "description": "来自 flexible_step3 的纯文本输出，包含可用字段和用户选中的字段"
                    }
                },
                "required": ["execution_trace", "output_format", "field_selection_result"]
            }),
            ChatConfig {
                system_prompt_template: Some("flexible/flexible_step4".into()),
                allowed_tools: Some(vec!["request_user_action".to_string()]),
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            None,
        );
        // flexible_step5：step5 落库回调（storage 就绪时才有；否则 None 仅记日志跳过）
        let plans_flexible_service = storage_repo(storage_resource, |ctx| {
            Arc::new(PlansFlexibleService::new(
                ctx.plans_flexible_repo(),
                ctx.plan_repo(),
                ctx.session_repo(),
            ))
        });
        let step5_callback = plans_flexible_service
            .map(|svc| {
                let slot = session_slot.read().clone();
                create_step5_callback(plan_id.clone(), svc, slot)
            })
            .flatten();
        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_step5",
            "灵活模式模板序列化 Agent：将需求澄清、执行轨迹、字段选择、参数化确认的结果编译为可复用的混合模板（steps 硬脚本 + execution_plan 智能说明书）。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task_definition": {
                        "type": "string",
                        "description": "来自 flexible_step1 的 Markdown 任务描述（含任务名称、参数列表）"
                    },
                    "execution_trace": {
                        "type": "string",
                        "description": "来自 flexible_step2 的完整执行轨迹（按顺序的工具调用及输入参数）"
                    },
                    "field_selection_result": {
                        "type": "string",
                        "description": "来自 flexible_step3 的纯文本输出，包含可用字段和用户选中的字段"
                    },
                    "parameter_confirmation_result": {
                        "type": "string",
                        "description": "来自 flexible_step4 的纯文本输出，包含参数候选、选中参数和模板输入定义"
                    }
                },
                "required": ["task_definition", "execution_trace", "field_selection_result", "parameter_confirmation_result"]
            }),
            ChatConfig {
                system_prompt_template: Some("flexible/flexible_step5".into()),
                allowed_tools: Some(vec!["request_user_action".to_string()]),
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            step5_callback,
        );
    });
    use_drop(move || {
        for name in [
            "flexible_step1",
            "flexible_step2",
            "flexible_step3",
            "flexible_step4",
            "flexible_step5",
        ] {
            let _ = registry.unregister_tool(name);
        }
    });

    FlexibleController {
        chat,
        thinking,
        temperature,
        template,
        templates,
        svc,
        factory,
        session_id,
        session_slot,
        svc_started,
        initialized,
    }
}
