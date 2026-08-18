//! ChatService 派生 hook

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use dioxus::prelude::*;
use planned_agent::{ChatConfig, ChatService};
use planned_agent::chat::SubAgentRunner;
use planned_agent_prompt_manager::FilePromptManager;
use planned_agent_tool_manager::ToolCategory;

use crate::context::{AiContext, PromptContext, ToolsContext};

use super::types::ChatServiceSignal;

/// 派生 Signal：缓存 `Arc<ChatService<FilePromptManager>>`。
///
/// `config`（含 `system_prompt_template` 与 `allowed_tools`）由**调用方**传入，
/// hook 内部不做任何模板/白名单判断。`ai` + `tools` + `prompt` 三者全 Ready 时
/// 用该 config 构建一次 service；之后**不重建**——模板热切换由调用方直接调用
/// `ChatService::set_system_prompt_template` / `set_allowed_tools` +
/// `reset_session` 完成（见 `pages/chat/page.rs`）。
///
/// `ChatService::new` 在构造时即解析 `AiClient`（返回 `Result`），
/// 解析失败（配置的 provider 不存在）时置 `None` 并记录错误日志。
pub(crate) fn use_chat_service(config: ChatConfig) -> ChatServiceSignal {
    let ai_resource = use_context::<Resource<Option<Arc<AiContext>>>>();
    let tools_resource = use_context::<Resource<Option<Arc<ToolsContext>>>>();
    let prompt_resource = use_context::<Resource<Option<Arc<PromptContext>>>>();

    let chat_signal: ChatServiceSignal = use_signal_sync(|| None);
    {
        let mut chat_signal = chat_signal;
        use_effect(move || {
            let ai = ai_resource.read().as_ref().and_then(|x| x.as_ref()).cloned();
            let tools = tools_resource.read().as_ref().and_then(|x| x.as_ref()).cloned();
            let prompt = prompt_resource.read().as_ref().and_then(|x| x.as_ref()).cloned();
            // 三者全 Ready 才构建；任一缺失则清空 Signal（保持 None）。
            let next = match (ai, tools, prompt) {
                (Some(ai), Some(tools), Some(p)) => {
                    let pm: Arc<FilePromptManager> = p.manager.clone();
                    match ChatService::new(
                        (*ai.manager).clone(),
                        tools.registry.clone(),
                        pm,
                        config.clone(),
                    ) {
                        Ok(svc) => Some(Arc::new(svc)),
                        Err(e) => {
                            tracing::error!("ChatService::new 失败: {}", e);
                            None
                        }
                    }
                }
                _ => None,
            };
            chat_signal.set(next);
        });
    }
    chat_signal
}

// ── 子 agent 注册 hook ────────────────────────────────────────────────────

/// 子 agent 注册幂等表：按 tool_name 去重。
fn registered_agents() -> &'static Mutex<HashMap<String, ()>> {
    static INSTANCE: OnceLock<Mutex<HashMap<String, ()>>> = OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 注册子 agent tool（幂等，同一 `tool_name` 只注册一次）。
///
/// 内部等待 `ai` + `tools` + `prompt` 三个 Resource 就绪后，
/// 用工厂参数构造 [`SubAgentRunner`] 并调用 `register_sub_agent`。
/// 后续每次 `runner.start()` 会新建独立的 `ChatService`，完成即 drop。
///
/// # 参数
///
/// - `tool_name`：暴露给 LLM 的工具名（如 `"demo_agent"`）
/// - `description`：工具描述（LLM 看到的）
/// - `schema`：工具 input_schema（JSON Schema）
/// - `config`：子 agent 的 `ChatConfig`（不含 `run_id`，每次调用时注入）
pub(crate) fn use_sub_agent(
    tool_name: &'static str,
    description: &'static str,
    schema: serde_json::Value,
    config: ChatConfig,
) {
    let ai_resource = use_context::<Resource<Option<Arc<AiContext>>>>();
    let tools_resource = use_context::<Resource<Option<Arc<ToolsContext>>>>();
    let prompt_resource = use_context::<Resource<Option<Arc<PromptContext>>>>();

    use_effect(move || {
        let ai = ai_resource.read().as_ref().and_then(|x| x.as_ref()).cloned();
        let tools = tools_resource.read().as_ref().and_then(|x| x.as_ref()).cloned();
        let prompt = prompt_resource.read().as_ref().and_then(|x| x.as_ref()).cloned();

        let (Some(ai), Some(ctx), Some(prompt)) = (ai, tools, prompt) else {
            return;
        };

        // 幂等检查：同一 tool_name 只注册一次
        {
            let mut registered = registered_agents().lock().unwrap();
            if registered.contains_key(tool_name) {
                return;
            }
            registered.insert(tool_name.to_string(), ());
        }

        let runner = SubAgentRunner::new(
            (*ai.manager).clone(),
            ctx.registry.clone(),
            prompt.manager.clone(),
            config.clone(),
            0,
            3,
        );

        let tool = planned_agent_core::mcp::types::Tool {
            name: tool_name.to_string(),
            description: description.to_string(),
            input_schema: schema.clone(),
        };
        ctx.registry.register_sub_agent(
            tool,
            vec![ToolCategory::Utility],
            Arc::new(runner),
        );
        tracing::info!("子 agent 已注册: {}", tool_name);
    });
}
