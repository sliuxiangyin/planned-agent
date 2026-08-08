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
/// `system_prompt_template` 为响应式信号：调用方通过 `use_memo` 将 UI 状态
/// （如 `plan_mode`）映射为模板路径（相对 `prompts/` 目录、不含 `.toml` 后缀，
/// 例如 `"thorough/thorough_system"`）。信号变化时 effect 自动重建 `ChatService`，
/// 实现运行时热切换 system prompt 模板。
///
/// 其余配置（`temperature`、`max_tokens`、`max_tool_rounds`、`enable_thinking`
/// 等）使用 `ChatConfig::default()`，可通过扩展本函数签名追加响应式参数。
///
/// 必须在组件渲染期间调用（依赖 Dioxus 当前 scope）。
pub(crate) fn use_chat_service(
    system_prompt_template: ReadSignal<Option<String>>,
) -> ChatServiceSignal {
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
            // 读信号使之成为 effect 依赖：template 变化 → effect 重跑 → ChatService 重建
            let template = system_prompt_template();
            // 三者全 Ready 才构建；任一缺失则清空 Signal（保持 None）。
            let next = match (ai, tools, prompt) {
                (Some(ai), Some(tools), Some(p)) => {
                    // ChatService<PM> 这里固定实参化为 FilePromptManager
                    // （与 planned-agent 同款风格的 prompt_manager: Arc<PM>）
                    let pm: Arc<FilePromptManager> = p.manager.clone();
                    // 周密模式（thorough/thorough_system）仅暴露 UI 交互工具，禁止直接执行
                    // Chat 测试页模板（chat/ 前缀）：仅暴露交互工具，便于调试 request_user_action
                    let allowed_tools = match template.as_deref() {
                        Some("thorough/thorough_system") => {
                            Some(vec!["request_user_action".to_string()])
                        }
                        Some(t) if t.starts_with("chat/") => {
                            Some(vec![
                                "request_user_action".to_string(),
                                "builtin_read_documentation".to_string(),
                            ])
                        }
                        _ => None, // 灵活模式或其他：全部工具可用
                    };
                    let config = ChatConfig {
                        system_prompt_template: template,
                        allowed_tools,
                        ..Default::default()
                    };
                    let svc: ChatService<FilePromptManager> = ChatService::new(
                        (*ai.manager).clone(),
                        tools.registry.clone(),
                        pm,
                        config,
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