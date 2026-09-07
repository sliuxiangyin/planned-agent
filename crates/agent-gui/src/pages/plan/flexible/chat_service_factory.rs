//! ChatService 创建工厂 —— 灵活模式页面如何为一个 session 构造「绑定该 store 的 ChatService」。
//!
//! 从 `controller.rs` 拆出，让「造服务」的工厂逻辑与「页面状态 / 事件编排」的
//! `FlexibleController` / `use_flexible_controller` 各居其位、互不堆叠。

use std::sync::Arc;

use planned_agent::chat::ChatConfig;
use planned_agent::ChatService;
use planned_agent_prompt_manager::FilePromptManager;

use crate::context::{AiContext, PromptContext, StorageContext, ToolsContext};

use super::chat_flexible_message_storage::ChatMessageStore;

/// 便捷类型：灵活模式所用 ChatService。
pub(crate) type ChatSvc = ChatService<FilePromptManager>;

/// ChatService 创建工厂：固化「造一个绑某 session store 的 ChatService」所需的依赖，
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
    pub(crate) fn new(
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
    pub(crate) async fn build_for_session(&self, session_id: &str) -> anyhow::Result<ChatSvc> {
        let repo = self.storage.chat_message_repo();
        let store = ChatMessageStore::new(self.plan_id.clone(), session_id.to_string(), repo);
        Ok(ChatService::with_store(
            self.ai.manager.default()?,
            self.tools.registry.clone(),
            self.prompt.manager.clone(),
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
                    "flexible_step_rua_demo".to_string(),
                ]),
                max_tool_rounds: 2,
                ..Default::default()
            },
            Arc::new(store),
        )
        .await)
    }
}
