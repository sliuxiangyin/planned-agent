//! 子 agent 会话：持有独立的 `ChatService`，支持挂起-恢复。

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use planned_agent_prompt_manager::FilePromptManager;
use planned_agent_tool_manager::{
    SubAgentRunOutcome, SubAgentSession, SubAgentSessionRunner, ToolStreamSender,
};
use serde_json::Value;

use crate::chat::service::ChatService;

use super::callback::SubAgentResultCallback;
use super::collect::collect_until_outcome;

/// 子 agent 会话：持有独立的 `ChatService`（由 `start()` 创建），支持 resume。
///
/// `ChatService` 内部维护 history（checkpoint），挂起时历史保留，
/// resume 时压入 tool 消息闭合协议后从历史继续。
pub struct ChatSubAgentSession {
    service: ChatService<FilePromptManager>,
    depth: u32,
    max_depth: u32,
    result_callback: Option<Arc<dyn SubAgentResultCallback>>,
}

impl ChatSubAgentSession {
    pub fn new(
        service: ChatService<FilePromptManager>,
        depth: u32,
        max_depth: u32,
        result_callback: Option<Arc<dyn SubAgentResultCallback>>,
    ) -> Self {
        Self {
            service,
            depth,
            max_depth,
            result_callback,
        }
    }
}

#[async_trait]
impl SubAgentSession for ChatSubAgentSession {
    async fn resume(
        &mut self,
        user_input: Value,
        stream: ToolStreamSender,
    ) -> Result<SubAgentRunOutcome> {
        // user_input 形如 {"choice": "...", "action_id": "..."}
        let choice = user_input
            .get("choice")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let action_id = user_input
            .get("action_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // 继续原对话：把选择结果作为 text 闭合挂起的 request_user_action，
        // 然后从 history 继续 run_conversation（不是 send 新消息）。
        let ticket = self
            .service
            .resume(&choice, &action_id)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        collect_until_outcome(
            &self.service,
            ticket,
            &stream,
            self.depth,
            self.max_depth,
            self.result_callback.clone(),
        )
        .await
    }
}
