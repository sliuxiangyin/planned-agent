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

use planned_agent::chat::{ChatConfig, PerRoundContext, SystemPrompt};
use planned_agent::ChatService;
use planned_agent_core::prompt::{PromptContext as PromptTemplateContext, PromptManager};
use planned_agent_prompt_manager::FilePromptManager;
use serde_json::json;

use crate::context::{AiContext, PromptContext, StorageContext, ToolsContext};
use crate::services::plans_flexible_service::PlansFlexibleService;

use super::chat_flexible_message_storage::ChatMessageStore;
use super::state_context::FlexibleStateContext;

/// 协调器 system prompt 模板名（与 PromptManager 中的注册名一致）。
const FLEXIBLE_GLOBAL_SYSTEM_PROMPT: &str = "flexible/flexible_global_system";

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
    // ── 每轮状态注入来源（必须在 `plan_id` / `session_id` 被 move 之前构造）──
    // 协调器的档位由系统直取、每轮注入，不靠 LLM 主动查（见
    // docs/planned-agent/flexible-coordinator-state-injection.md）。
    let state_context = FlexibleStateContext::new(
        plan_id.clone(),
        session_id.clone(),
        Arc::new(PlansFlexibleService::new(
            storage.plans_flexible_sessions_repo(),
            storage.flexible_state_repo(),
        )),
    );

    let repo = storage.chat_message_repo();
    let store = ChatMessageStore::new(plan_id, session_id.clone(), repo);

    // 协调器 system prompt：以 PromptContext 注入 session_id 渲染 flexible_global_system
    // （模板正文用 `{{ session_id }}`），再以 SystemPrompt::Rendered 注入 —— 因为核心 driver 的
    // Template 分支渲染时用空 PromptContext、带不了变量，故「带变量的渲染」须在 GUI 侧完成。
    // 每个会话自己的 config → session_id 天然 per-session 隔离。
    let context = PromptTemplateContext::new().with_variable("session_id", json!(session_id));
    let coordinator_system_prompt = prompt
        .manager
        .render(FLEXIBLE_GLOBAL_SYSTEM_PROMPT, &context)
        .await
        .map_err(|e| anyhow::anyhow!("渲染协调器 system prompt 失败: {e}"))?;

    Ok(ChatService::with_store(
        ai.manager.default()?,
        tools.registry.clone(),
        prompt.manager.clone(),
        ChatConfig {
            system_prompt: Some(SystemPrompt::Rendered(coordinator_system_prompt)),
            // 每轮把本会话真实状态注入协调器上下文（临时 system 消息，不写 history）。
            per_round_context: Some(PerRoundContext::new(state_context)),
            // 协调器仅做状态机调度，不执行业务：工具层只暴露全部 step 子 agent（含修订）
            // + flexible_state（只读）+ request_user_action，杜绝误调业务 / 其它子 agent 工具。
            allowed_tools: Some(vec![
                "flexible_clarify".to_string(),
                "flexible_plan".to_string(),
                "flexible_parameterize".to_string(),
                "flexible_output".to_string(),
                "flexible_save".to_string(),
                "flexible_revise".to_string(),
                "flexible_state".to_string(),
                "request_user_action".to_string(),
            ]),
            max_tool_rounds: 10,
            ..Default::default()
        },
        Arc::new(store),
    )
    .await)
}
