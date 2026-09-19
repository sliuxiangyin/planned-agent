//! 灵活模式主组件（多会话保活）。
//!
//! 结构：
//! - `FlexiblePage`（壳）：维护「已打开会话集合」，把每个会话渲染成一个常驻
//!   `FlexibleSessionHost`；当前 active 由 `SessionManager` 决定。**plan 级共享的资源**
//!   （子 agent / 工具注册、模板列表）在此处一次性建立（见 `use_plan_agent_registrations`
//!   / `use_plan_templates`）。
//! - `FlexibleSessionHost`（常驻宿主）：每个会话一个，各自 boot 一次并把
//!   `ChatBridge`（内含 `ChatService`）留在组件里 —— 切换只改「谁 active」，
//!   不卸载任何宿主，后台会话的 driver 持续收流/落库（保活）。
//!
//! 设计见 `docs/chat-flexible-多会话保活设计.md` §2 / §3 A / §3 B。

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::chat::{ChatConfig, SystemPrompt};
use planned_agent_core::prompt::PromptManager;
use planned_agent_core::tool_registry::ToolCategory;

use crate::context::{
    register_sub_agent, require_resource, AiContext, PromptContext, StorageContext, ToolsContext,
};
use crate::pages::plan::shared::session::SessionManager;
use crate::services::plans_flexible_service::PlansFlexibleService;

use super::session_host::FlexibleSessionHost;
use super::step_callback::{
    create_save_callback, create_save_inject, create_step1_callback, create_step1_inject,
    create_step2_callback, create_step2_inject, HOST_SESSION_ID_FIELD,
};
use super::tool::{flexible_state_tool, FlexibleStateExecutor};

#[derive(Props, Clone, PartialEq)]
pub struct FlexiblePageProps {
    pub plan_id: String,
    pub session_id: String,
}

/// 灵活模式「壳」：维护已打开会话集合并渲染各会话宿主。
#[component]
pub fn FlexiblePage(props: FlexiblePageProps) -> Element {
    // 会话状态管理中心（PlanPage 已注入 context）：当前 active 会话。
    let session_mgr = use_context::<Arc<SessionManager>>();
    let active = session_mgr.current(); // Signal<Option<String>>

    // 已打开会话集合：首帧含 props.session_id；每出现新 active 就 ensure 加入。
    // （本阶段只增不减；LRU 回收见设计文档 §5 决策 1。）
    // 注：仅用 props.session_id 作初值；调用方保证 plan 就绪后才渲染本组件。
    let open = use_signal_sync({
        let init = props.session_id.clone();
        move || {
            if init.is_empty() {
                Vec::<String>::new()
            } else {
                vec![init]
            }
        }
    });

    let mut open_ensure = open; // Copy 句柄
    let active_listen = active;
    use_effect(move || {
        if let Some(id) = active_listen.read().clone() {
            if !id.is_empty() {
                let mut o = open_ensure.write();
                if !o.contains(&id) {
                    o.push(id);
                }
            }
        }
    });

    // ── plan 级共享：子 agent / 工具注册（一次性，壳挂载时建立、卸载时注销）──
    use_plan_agent_registrations(props.plan_id.clone());

    // ── plan 级共享：可用模板列表（所有会话同一份）──
    let templates = use_plan_templates();

    // 当前 active 会话 id（无当前会话时回退到 props 初值）。
    let active_id = active
        .read()
        .clone()
        .unwrap_or_else(|| props.session_id.clone());
    let sessions = open.read().clone();

    rsx! {
        div { class: "flexible-page",
            for sid in sessions.iter().cloned() {
                FlexibleSessionHost {
                    key: "{sid}",
                    plan_id: props.plan_id.clone(),
                    session_id: sid.clone(),
                    active: sid == active_id,
                    templates,
                }
            }
        }
    }
}

/// plan 级注册（自定义 hook）：只在壳挂载时执行一次，壳卸载时注销。
///
/// 原在 `use_flexible_controller`（每 host 各跑一次）——多会话下会重复注册、
/// 且任一 host 卸载会注销全局工具；上移到壳后与「plan 生命周期」对齐。
fn use_plan_agent_registrations(plan_id: String) {
    let storage_ctx = require_resource::<StorageContext>();
    let ai_ctx = require_resource::<AiContext>();
    let tools_ctx = require_resource::<ToolsContext>();
    let prompt_ctx = require_resource::<PromptContext>();
    let registry = tools_ctx.registry.clone();

    use_hook(move || {
        // flexible_save 落库回调 + flexible_state 工具（storage 由启动门保证就绪，始终存在）
        let plans_flexible_service = Arc::new(PlansFlexibleService::new(
            storage_ctx.plans_flexible_sessions_repo(),
            storage_ctx.flexible_state_repo(),
        ));
        // flexible_state：协调器读写「流程中间状态」的旁路工具（session_id 由协调器经参数传入）。
        {
            let executor =
                FlexibleStateExecutor::new(plan_id.clone(), plans_flexible_service.clone());
            tools_ctx.register_custom_tool(
                flexible_state_tool(),
                vec![ToolCategory::Utility],
                Arc::new(executor),
            );
        }

        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_step1",
            "需求澄清子 Agent：将用户自然语言需求澄清为可执行的任务定义（只澄清需求本身，不分析输入参数或输出形式）。",
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
                    },
                    "previous_task_definition": {
                        "type": "object",
                        "description": "任务基线（task_definition 对象，含 task）。**由系统自动注入**（取自本会话 flexible_state 的已定稿产物，是基线的唯一来源），你无需传；首次澄清时系统不注入，你也不必补"
                    },
                    "host_session_id": {
                        "type": "string",
                        "description": "本会话 ID，原样照抄 system prompt「会话上下文」中给出的值，不得改写"
                    }
                },
                "required": ["user_message", "host_session_id"]
            }),
            ChatConfig {
                system_prompt: Some(SystemPrompt::Template("flexible/flexible_step1".into())),
                allowed_tools: Some(vec!["request_user_action".to_string()]),
                // host_session_id 是宿主注入的控制字段（供回调定位会话），不进子 agent 的 task 文本。
                hidden_args: vec![HOST_SESSION_ID_FIELD.to_string()],
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            create_step1_callback(plan_id.clone(), plans_flexible_service.clone()),
            // 任务基线由系统从 flexible_state 注入（映射见 step1/mod.rs 的 INJECT_MAPPING），
            // 不再依赖协调器转抄。
            vec![create_step1_inject(
                plan_id.clone(),
                plans_flexible_service.clone(),
            )],
        );
        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_step2",
            "参数提取子 Agent：从需求澄清结果中识别可变参数，产出可复用的参数化任务（parameterized_task：占位符模板 + 参数表）。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task_definition": {
                        "type": "object",
                        "description": "任务定义对象（含 task 任务描述）。**由系统自动注入**（取自本会话 flexible_state 的已定稿产物），你无需传"
                    },
                    "host_session_id": {
                        "type": "string",
                        "description": "本会话 ID，原样照抄 system prompt「会话上下文」中给出的值，不得改写"
                    }
                },
                "required": ["host_session_id"]
            }),
            ChatConfig {
                system_prompt: Some(SystemPrompt::Template("flexible/flexible_step2".into())),
                // 参数提取是纯文本分析，不调用任何工具（不执行、不交互）。
                allowed_tools: Some(vec![]),
                // host_session_id 是宿主注入的控制字段（供回调定位会话），不进子 agent 的 task 文本。
                hidden_args: vec![HOST_SESSION_ID_FIELD.to_string()],
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            create_step2_callback(plan_id.clone(), plans_flexible_service.clone()),
            // 任务定义由系统注入（见 step2/mod.rs 的 INJECT_MAPPING）。
            vec![create_step2_inject(
                plan_id.clone(),
                plans_flexible_service.clone(),
            )],
        );
        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_save",
            "保存子 Agent：校验参数提取结果（parameterized_task），确认后落库为可复用模板。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "parameterized_task": {
                        "type": "object",
                        "description": "step2 定稿的参数提取结果（含 template 与 parameters）。**由系统自动注入**，你无需传"
                    },
                    "host_session_id": {
                        "type": "string",
                        "description": "本会话 ID，原样照抄 system prompt「会话上下文」中给出的值，不得改写"
                    }
                },
                "required": ["host_session_id"]
            }),
            ChatConfig {
                system_prompt: Some(SystemPrompt::Template("flexible/flexible_save".into())),
                allowed_tools: Some(vec![]),
                // host_session_id 是宿主注入的控制字段（供回调定位会话），不进子 agent 的 task 文本。
                hidden_args: vec![HOST_SESSION_ID_FIELD.to_string()],
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            // 定稿回调直接落库（读 state 的 parameterized_task，不经协调器转抄）并推进 saved。
            create_save_callback(plan_id.clone(), plans_flexible_service.clone()),
            // parameterized_task 由系统注入（见 save/mod.rs 的 INJECT_MAPPING）。
            vec![create_save_inject(
                plan_id.clone(),
                plans_flexible_service.clone(),
            )],
        );
    });

    use_drop(move || {
        for name in [
            "flexible_step1",
            "flexible_step2",
            "flexible_save",
            "flexible_state",
        ] {
            let _ = registry.unregister_tool(name);
        }
    });
}

/// plan 级共享：加载可用模板列表（`chat/` 与 `flexible/` 前缀）。
fn use_plan_templates() -> Signal<Vec<String>, SyncStorage> {
    let prompt_ctx = require_resource::<PromptContext>();
    let mut templates = use_signal_sync(|| Vec::<String>::new());
    use_effect(move || {
        let prompt = prompt_ctx.clone();
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
    templates
}
