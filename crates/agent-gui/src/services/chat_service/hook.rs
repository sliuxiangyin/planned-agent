//! ChatService 派生 hook

use std::sync::Arc;
use dioxus::prelude::*;
use planned_agent::{ChatConfig, ChatService};
use planned_agent_prompt_manager::FilePromptManager;

use crate::context::{AiContext, PromptContext, ToolsContext};

use super::types::ChatServiceSignal;

/// 派生 Signal：根据 `ai_resource` + `tools_resource` + `prompt_resource` 当前值，
/// 缓存一个 `Arc<ChatService<FilePromptManager>>`。三者全 Ready 时构建；
/// 任何一项缺失则返回 `None`，等待下一次 effect 触发重建。
///
/// `ChatService::new` 需要 owned `AiManager`，故从 `Arc<AiContext>` clone 出 owned 副本
/// （`AiManager: Clone`，仅 Arc 引用 +1，client 本身不深拷贝）。
///
/// `chat_config` 由调用方传入：可定制 `system_prompt_template`（模板路径，相对
/// `prompts/` 目录、不含 `.toml` 后缀，例如 `"chat/system"`）、`provider`、
/// `temperature`、`max_tokens`、`max_tool_rounds`、`enable_thinking` 等。
/// `ChatService` 内部会通过 `PromptManager::render(template, ctx)` 渲染模板。
///
/// 用 `use_signal_sync + use_effect` 组合，绕开 `use_memo` 对 `PartialEq` 的要求。
///
/// 必须在组件渲染期间调用（依赖 Dioxus 当前 scope）。
pub(crate) fn use_chat_service(chat_config: ChatConfig) -> ChatServiceSignal {
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
                    // ChatService<PM> 这里固定实参化为 FilePromptManager
                    // （与 planned-agent 同款风格的 prompt_manager: Arc<PM>）
                    let pm: Arc<FilePromptManager> = p.manager.clone();
                    let svc = ChatService::new(
                        (*ai.manager).clone(),
                        tools.registry.clone(),
                        pm,
                        chat_config.clone(),
                    );
                    Some(Arc::new(svc))
                }
                _ => None,
            };
            chat_signal.set(next);
        });
    }
    chat_signal
}