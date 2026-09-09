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
use std::sync::Arc;

use anyhow::anyhow;
use dioxus::prelude::*;
use planned_agent::chat::{ChatConfig, SubscriptionGuard};
use planned_agent_core::tool_registry::ToolCategory;

use crate::components::chat::chat_flow::{handle_user_action, Bubble, ChatSignals, PendingUI};
use crate::context::{
    register_sub_agent, require_resource, AiContext, PromptContext, StorageContext, ToolsContext,
};
use crate::pages::plan::shared::session::SessionManager;
use crate::services::plans_flexible_service::PlansFlexibleService;

use super::chat_service_factory::ChatSvc;
use super::flexible_state_tool::{flexible_state_tool, FlexibleStateExecutor};
use super::session_boot::{boot_flexible_session, FlexBoot, OnBootPhase};
use super::step2_callback::create_step2_callback;
use super::step5_callback::create_step5_callback;

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
    /// 会话启动状态机：Loading(进度) → Ready(ReadySession) / Failed。
    ///
    /// 取代旧的多份分散 signal（svc / svc_started / initialized / session_mgr）。
    boot: Signal<FlexBoot, SyncStorage>,
}

impl FlexibleController {
    /// 就绪后的 ChatService；未就绪/失败时为 None。
    pub(crate) fn service(&self) -> Option<Arc<ChatSvc>> {
        match self.boot.read().as_ref() {
            FlexBoot::Ready(session) => Some(session.svc.clone()),
            _ => None,
        }
    }

    /// 当前会话启动状态（供页面按 Loading / Failed / Ready 分支渲染）。
    pub(crate) fn boot_phase(&self) -> FlexBoot {
        self.boot.read().clone()
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

    /// 用户提交 request_user_action / 子 agent 卡片后的回调。
    pub(crate) fn on_user_action(&self, choice: String, pending: PendingUI) {
        let Some(svc) = self.service() else { return };
        let mut chat = self.chat;
        handle_user_action(&mut chat, &svc, choice, pending);
    }

    /// 切换到指定 session（多会话并行改造前的占位）。
    ///
    /// 原实现会停掉旧对话、为它新建一个绑目标 session store 的 ChatService 并重放
    /// 历史；该逻辑依赖已移除的 `ChatServiceFactory` struct 且在并发切换下有硬伤
    /// （切走即 stop 当前会话）。多会话架构就位后将改为「切走不 stop、后台继续跑」。
    #[allow(dead_code)] // 预留 API：待"历史翻回"UI 接线后使用
    pub(crate) fn switch_session(&self, _session_id: String) -> anyhow::Result<()> {
        // TODO(多会话并行): ChatServiceFactory struct 已移除；此处改为直接持有依赖
        // （storage/ai/tools/prompt）按需构造新会话 ChatService，并在多会话架构就位后
        // 改为「切走不 stop、后台继续跑」。现临时占位，避免误用单会话重建逻辑。
        Err(anyhow!("switch_session 尚未就绪：待多会话并行改造完成后接入"))
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
    let chat = ChatSignals {
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

    // ── 会话启动状态机（boot.rs 风格）：Loading → Ready / Failed ──
    let mut boot = use_signal_sync(|| FlexBoot::Loading(Vec::new()));
    let mut templates = use_signal_sync(|| Vec::<String>::new());
    // plan 级共享单例（PlanPage 已注入 context）；flexible_state/step5 旁路与 boot 广播用
    let session_mgr_ctx = use_context::<Arc<SessionManager>>();
    let session_mgr = use_signal_sync(move || session_mgr_ctx.clone());
    // boot 幂等门：仅首次 render 触发一次异步启动
    let mut boot_started = use_signal_sync(|| false);

    // ── 依赖 context（启动门保证全部就绪，require_resource 直接返回 Arc）──
    let storage_ctx = require_resource::<StorageContext>();
    let ai_ctx = require_resource::<AiContext>();
    let tools_ctx = require_resource::<ToolsContext>();
    let prompt_ctx = require_resource::<PromptContext>();

    // ── 供异步 boot 闭包捕获的 owned clone ──
    let plan_id_c = plan_id.clone();
    let ai_ctx_c = ai_ctx.clone();
    let tools_ctx_c = tools_ctx.clone();
    let prompt_ctx_c = prompt_ctx.clone();
    let storage_ctx_c = storage_ctx.clone();

    // ── 会话异步启动（一次）：建 session → new_chat_service → driver → 历史 → 订阅 ──
    use_effect(move || {
        if *boot_started.read() {
            return;
        }
        boot_started.set(true);

        let storage = storage_ctx_c.clone();
        let plan_id = plan_id_c.clone();
        let ai_ctx = ai_ctx_c.clone();
        let tools_ctx = tools_ctx_c.clone();
        let prompt_ctx = prompt_ctx_c.clone();
        let slot = session_mgr.read().clone();
        // ChatSignals 是 Copy：把句柄副本交给 boot 异步填充历史/订阅
        let chat_boot = chat;
        let boot_done = boot;
        let boot_progress = boot;
        // 进度回调：把已完成阶段名累积进 boot Loading（对齐全局 boot.rs 的 on_progress）
        let progress_cb: OnBootPhase = Arc::new(move |name: &'static str| {
            let done = if let FlexBoot::Loading(d) = boot_progress.read().clone() {
                d
            } else {
                return;
            };
            if !done.contains(&name) {
                let mut d = done;
                d.push(name);
                boot_progress.set(FlexBoot::Loading(d));
            }
        });
        spawn(async move {
            match boot_flexible_session(
                storage,
                plan_id,
                ai_ctx,
                tools_ctx,
                prompt_ctx,
                chat_boot,
                slot,
                progress_cb,
            )
            .await
            {
                Ok(session) => boot_done.set(FlexBoot::Ready(Arc::new(session))),
                Err(errors) => {
                    tracing::error!("灵活模式会话启动失败: {:?}", errors);
                    boot_done.set(FlexBoot::Failed(errors));
                }
            }
        });
    });

    // ── 可用模板列表 ──
    let prompt_ctx_t = prompt_ctx.clone();
    use_effect(move || {
        let prompt = prompt_ctx_t.clone();
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
    });

    // ── 注册子 agent（生命周期方案，避免重复注册与孤儿工具）──
    let registry = tools_ctx.registry.clone();
    use_hook(move || {
        // flexible_step5 落库回调 + flexible_state 工具（storage 由启动门保证就绪，始终存在）
        let plans_flexible_service = Arc::new(PlansFlexibleService::new(
            storage_ctx.plans_flexible_repo(),
            storage_ctx.plan_repo(),
            storage_ctx.session_repo(),
            storage_ctx.flexible_state_repo(),
        ));
        // flexible_state：协调器读写「当前会话流程中间状态」的旁路工具。
        // 按引用借用再 clone，避免 move 掉 plans_flexible_service（下文 step5 回调仍要读它）。
        {
            let receiver = session_mgr.read().clone().receiver();
            let executor = FlexibleStateExecutor::new(
                plan_id.clone(),
                plans_flexible_service.clone(),
                receiver,
            );
            tools_ctx.register_custom_tool(
                flexible_state_tool(),
                vec![ToolCategory::Utility],
                Arc::new(executor),
            );
        }

        // 测试用：子 agent 连续多次 request_user_action 交互的专用子 agent（可选注册，供 GUI 测试）。
        // 由引导模板 chat/sub_agent_rua_driver.toml 驱动的协调器去调用它。
        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_step_rua_demo",
            "request_user_action 子 agent 交互测试：被调用后在子 agent 内连续向用户发起多次 request_user_action（用于在 GUI 验证子 agent 内交互卡的渲染/回传/取消/回显）。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "user_message": {
                        "type": "string",
                        "description": "来自协调器的测试指令（一般无需传业务内容）"
                    }
                },
                "required": []
            }),
            ChatConfig {
                system_prompt_template: Some("chat/sub_agent_rua_demo".into()),
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
                // step2 是纯业务执行：用 "all" 剔除 Utility/SubAgent（含 flexible_state、兄弟 step 子 agent），
                // 只暴露业务工具，避免执行 agent 误碰协调层工具。
                allowed_tools: Some(vec!["all".to_string()]),
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
                        "description": "来自 flexible_step1 的输出格式，已由用户确认（如 CSV、JSON、Markdown、文本等）"
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
                        "description": "来自 flexible_step1 的输出格式，已由用户确认（如 CSV、JSON、Markdown、文本等）"
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

        let step5_callback = {
            let receiver = session_mgr.read().clone().receiver();
            create_step5_callback(plan_id.clone(), plans_flexible_service.clone(), receiver)
        };
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
            "flexible_state",
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
        boot,
    }
}
