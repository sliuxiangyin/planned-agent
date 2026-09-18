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
use planned_agent::chat::{ChatConfig, SubAgentResultChain, SystemPrompt};
use planned_agent_core::prompt::PromptManager;
use planned_agent_core::tool_registry::ToolCategory;

use crate::context::{
    register_sub_agent, require_resource, AiContext, PromptContext, StorageContext, ToolsContext,
};
use crate::pages::plan::shared::session::SessionManager;
use crate::services::plans_flexible_service::PlansFlexibleService;

use super::session_host::FlexibleSessionHost;
use super::step_callback::{
    create_step1_callback, create_step2_callback, create_step3_callback, create_step4_callback,
    create_step5_callback, StateInjectCallback, HOST_SESSION_ID_FIELD,
};
use super::tool::{
    flexible_state_tool, flexible_save_template, FlexibleStateExecutor, FlexibleSaveTemplateExecutor,
};

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
        // flexible_step5 落库回调 + flexible_state 工具（storage 由启动门保证就绪，始终存在）
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
        // flexible_save_template：step5 产出后由协调器显式调用，登记模板快照（session_id 经参数传入）。
        {
            let executor =
                FlexibleSaveTemplateExecutor::new(plan_id.clone(), plans_flexible_service.clone());
            tools_ctx.register_custom_tool(
                flexible_save_template(),
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
                system_prompt: Some(SystemPrompt::Template("chat/sub_agent_rua_demo".into())),
                allowed_tools: Some(vec!["request_user_action".to_string()]),
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            SubAgentResultChain::default(), // 结果链：测试用子 agent 不需要
            vec![], // before 回调链：测试用子 agent 不需要注入
        );
        // 测试用：子 agent max_tool_rounds 触顶复现。
        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_step_max_rounds_demo",
            "子 agent max_tool_rounds 触顶复现测试：被调用后在子 agent 内持续调用 builtin_read_documentation 直到轮次上限触顶（用于在 GUI 验证子 agent 触顶时「是否继续」卡的挂起/恢复/回显）。",
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
                system_prompt: Some(SystemPrompt::Template(
                    "chat/sub_agent_max_rounds_demo".into(),
                )),
                allowed_tools: Some(vec!["builtin_read_documentation".to_string()]),
                max_tool_rounds: 2,
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            SubAgentResultChain::default(), // 结果链：测试用子 agent 不需要
            vec![], // before 回调链：测试用子 agent 不需要注入
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
                    },
                    "previous_task_definition": {
                        "type": "object",
                        "description": "可选：上一轮已得到的任务基线（task_definition 对象）。优先取 flexible_state 中已定稿的 task_definition，其次取最近一次 flexible_step1 返回的 task_definition；首次澄清时不传"
                    },
                    "previous_output_format": {
                        "type": "string",
                        "description": "可选：上一轮已得到的输出格式，与 previous_task_definition 配套；首次澄清时不传"
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
            vec![],
        );
        register_sub_agent(
            &ai_ctx,
            &tools_ctx,
            &prompt_ctx,
            "flexible_step2",
            "灵活模式任务执行 Agent：根据需求澄清结果执行工具调用。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "task_definition": {
                        "type": "object",
                        "description": "来自 flexible_step1 返回 JSON 的 task_definition 对象（含 task 任务描述与 params 参数）"
                    },
                    "runtime_context": {
                        "type": "string",
                        "description": "可选，来自上一轮执行的 compressed_context；首次执行时为空"
                    },
                    "host_session_id": {
                        "type": "string",
                        "description": "本会话 ID，原样照抄 system prompt「会话上下文」中给出的值，不得改写"
                    }
                },
                "required": ["task_definition", "host_session_id"]
            }),
            ChatConfig {
                system_prompt: Some(SystemPrompt::Template("flexible/flexible_step2".into())),
                // step2 是纯业务执行：用 "all" 剔除 Utility/SubAgent（含 flexible_state、兄弟 step 子 agent），
                // 只暴露业务工具，避免执行 agent 误碰协调层工具。
                allowed_tools: Some(vec!["all".to_string()]),
                // host_session_id 是宿主注入的控制字段（供回调定位会话），不进子 agent 的 task 文本。
                hidden_args: vec![HOST_SESSION_ID_FIELD.to_string()],
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            create_step2_callback(plan_id.clone(), plans_flexible_service.clone()),
            vec![],
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
                    },
                    "host_session_id": {
                        "type": "string",
                        "description": "本会话 ID，原样照抄 system prompt「会话上下文」中给出的值，不得改写"
                    }
                },
                "required": ["execution_trace_summary", "output_format", "host_session_id"]
            }),
            ChatConfig {
                system_prompt: Some(SystemPrompt::Template("flexible/flexible_step3".into())),
                allowed_tools: Some(vec!["request_user_action".to_string()]),
                // host_session_id 是宿主注入的控制字段（供回调定位会话），不进子 agent 的 task 文本。
                hidden_args: vec![HOST_SESSION_ID_FIELD.to_string()],
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            create_step3_callback(plan_id.clone(), plans_flexible_service.clone()),
            // 轨迹摘要从 flexible_state 直取注入，取代「协调器 LLM 转抄」：
            // 合并语义下注入方赢，故协调器即使仍传该字段也污染不了。
            vec![Arc::new(StateInjectCallback::new(
                plan_id.clone(),
                plans_flexible_service.clone(),
                vec![("compressed_context", "execution_trace_summary")],
            ))],
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
                        "type": "array",
                        "description": "来自 flexible_step2 会话历史导出的真实工具调用轨迹（每条含 id / name / arguments / output / outcome），包含每次工具调用的输入参数"
                    },
                    "output_format": {
                        "type": "string",
                        "description": "来自 flexible_step1 的输出格式，已由用户确认（如 CSV、JSON、Markdown、文本等）"
                    },
                    "field_selection_result": {
                        "type": "object",
                        "description": "来自 flexible_step3 返回 JSON 的 field_selection_result 对象（含 output_format / available_fields / selected_fields）"
                    },
                    "host_session_id": {
                        "type": "string",
                        "description": "本会话 ID，原样照抄 system prompt「会话上下文」中给出的值，不得改写"
                    }
                },
                "required": ["execution_trace", "output_format", "field_selection_result", "host_session_id"]
            }),
            ChatConfig {
                system_prompt: Some(SystemPrompt::Template("flexible/flexible_step4".into())),
                allowed_tools: Some(vec!["request_user_action".to_string()]),
                // host_session_id 是宿主注入的控制字段（供回调定位会话），不进子 agent 的 task 文本。
                hidden_args: vec![HOST_SESSION_ID_FIELD.to_string()],
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            create_step4_callback(plan_id.clone(), plans_flexible_service.clone()),
            // 执行轨迹从 flexible_state 直取注入，取代「协调器 LLM 转抄」。
            vec![Arc::new(StateInjectCallback::new(
                plan_id.clone(),
                plans_flexible_service.clone(),
                vec![("execution_trace", "execution_trace")],
            ))],
        );
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
                        "type": "object",
                        "description": "来自 flexible_step1 返回 JSON 的 task_definition 对象（含 task 任务描述与 params 参数）"
                    },
                    "execution_trace": {
                        "type": "array",
                        "description": "来自 flexible_step2 会话历史导出的真实工具调用轨迹（按声明顺序，每条含 id / name / arguments / output / outcome）"
                    },
                    "field_selection_result": {
                        "type": "object",
                        "description": "来自 flexible_step3 返回 JSON 的 field_selection_result 对象（含 output_format / available_fields / selected_fields）"
                    },
                    "parameter_confirmation_result": {
                        "type": "object",
                        "description": "来自 flexible_step4 返回 JSON 的 parameter_confirmation_result 对象（含 candidates / selected_params / template_input）"
                    },
                    "host_session_id": {
                        "type": "string",
                        "description": "本会话 ID，原样照抄 system prompt「会话上下文」中给出的值，不得改写"
                    }
                },
                "required": ["task_definition", "execution_trace", "field_selection_result", "parameter_confirmation_result", "host_session_id"]
            }),
            ChatConfig {
                system_prompt: Some(SystemPrompt::Template("flexible/flexible_step5".into())),
                allowed_tools: Some(vec!["request_user_action".to_string()]),
                // host_session_id 是宿主注入的控制字段（供回调定位会话），不进子 agent 的 task 文本。
                hidden_args: vec![HOST_SESSION_ID_FIELD.to_string()],
                ..Default::default()
            },
            1, // depth
            2, // max_depth
            // 回调只推进 current_step="templated"；模板落库仍由 flexible_save_template 工具负责。
            create_step5_callback(plan_id.clone(), plans_flexible_service.clone()),
            vec![],
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
            "flexible_save_template",
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

