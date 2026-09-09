//! ChatService 构造函数 —— 灵活模式页面如何为一个 session 构造「绑定该 store 的 ChatService」。
//!
//! 从 `controller.rs` 拆出，让「造服务」的构造逻辑与「页面状态 / 事件编排」的
//! `FlexibleController` / `use_flexible_controller` 各居其位、互不堆叠。
//!
//! 说明：`ChatServiceFactory` 原先是持有 5 个依赖 Arc、可复用于任意 session 的
//! struct（先 `new` 存依赖、再 `build_for_session(sid)` 反复造不同会话的服务）。
//! 该设计已被弃用 —— 多会话并行下每次「为某会话造服务」都直接调用本构造函数即可，
//! 无需保留一个跨会话复用的工厂实例，故简化为单个顶层异步构造函数：
//! 一次性收全依赖 + `session_id`，直接返回绑定该会话 store 的 `ChatService`。

use std::sync::Arc;

use planned_agent::chat::ChatConfig;
use planned_agent::ChatService;
use planned_agent_prompt_manager::FilePromptManager;

use crate::context::{AiContext, PromptContext, StorageContext, ToolsContext};

use super::chat_flexible_message_storage::ChatMessageStore;

/// 便捷类型：灵活模式所用 ChatService。
pub(crate) type ChatSvc = ChatService<FilePromptManager>;

/// 为指定 session 造一个绑定该 session store 的 ChatService（未 start_driver）。
///
/// 依赖均为 Arc（便宜 clone），调用方持有一份即可反复为不同 `session_id` 构造，
/// 因此不必再封装成一个可复用的工厂 struct。
#[allow(clippy::too_many_arguments)] // 一次性收全构造依赖，避免再引入状态容器
pub(crate) async fn new_chat_service(
    storage: Arc<StorageContext>,
    plan_id: String,
    session_id: String,
    ai: Arc<AiContext>,
    tools: Arc<ToolsContext>,
    prompt: Arc<PromptContext>,
) -> anyhow::Result<ChatSvc> {
    let repo = storage.chat_message_repo();
    let store = ChatMessageStore::new(plan_id, session_id, repo);
    Ok(ChatService::with_store(
        ai.manager.default()?,
        tools.registry.clone(),
        prompt.manager.clone(),
        ChatConfig {
            system_prompt_template: Some("flexible/flexible_global_system".to_string()),
            // 协调器仅做状态机调度，不执行业务：工具层只暴露 5 个 step 子 agent +
            // flexible_state + request_user_action，杜绝误调业务 / 其它子 agent 工具。
            //
            // ⚠️ 以下两项是「轮次触顶」复现测试的临时改动，测试完请还原：
            //   1) "builtin_read_documentation"：临时放开的一个简单无副作用工具，
            //      配合 chat/max_rounds_demo 模板让协调器持续调用工具直到触顶。
            //      正常使用应删除该条目。
            //   2) max_tool_rounds: 2：把轮次上限压到很小，2 轮即可触顶。
            //      正常协调器要调度 step1~step5 需要多轮，默认应为 10（删除这行即回默认）。
            allowed_tools: Some(vec![
                // "flexible_step1".to_string(),
                // "flexible_step2".to_string(),
                // "flexible_step3".to_string(),
                // "flexible_step4".to_string(),
                // "flexible_step5".to_string(),
                // "flexible_state".to_string(),
                // "request_user_action".to_string(),
                // "builtin_read_documentation".to_string(),
                // 测试用：供 chat/sub_agent_rua_driver.toml 驱动的协调器调用，
                // 验证子 agent 内 request_user_action 交互。测试完可一并删除。
                // "flexible_step_rua_demo".to_string(),
                // 测试用：子 agent max_tool_rounds 触顶复现（chat/sub_agent_max_rounds_driver.toml 驱动的协调器调用）。
                "flexible_step_max_rounds_demo".to_string(),
            ]),
            max_tool_rounds: 2,
            ..Default::default()
        },
        Arc::new(store),
    )
    .await)
}
