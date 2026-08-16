//! ChatService 派生 hook

use std::sync::Arc;
use dioxus::prelude::*;
use planned_agent::{ChatConfig, ChatService};
use planned_agent_prompt_manager::FilePromptManager;

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
