//! 子 agent Runner：持有工厂参数，每次 `start()` 新建独立 `ChatService`。

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use planned_agent_ai_manager::AiManager;
use planned_agent_core::mcp::types::ToolResult;
use planned_agent_prompt_manager::FilePromptManager;
use planned_agent_tool_manager::{
    SubAgentRunOutcome, SubAgentSessionRunner, ToolRegistry, ToolStreamSender,
};
use serde_json::Value;
use tracing::info;

use crate::chat::service::{ChatConfig, ChatService};

use super::callback::SubAgentResultCallback;
use super::collect::collect_until_outcome;

/// 子 agent runner：持有 `ChatService` 工厂参数，每次 `start()`
/// 新建独立 `ChatService`，完成即 drop，天然隔离且 driver loop
/// 随 `Arc<State>` 归零自动退出。
///
/// - `depth`：当前嵌套深度（0 = 顶层）
/// - `max_depth`：最大允许嵌套深度（防递归）
pub struct SubAgentRunner {
    ai_manager: AiManager,
    tool_registry: Arc<ToolRegistry>,
    prompt_manager: Arc<FilePromptManager>,
    /// 子 agent 专属配置（不含 run_id，run_id 每次调用时注入）
    config: ChatConfig,
    depth: u32,
    max_depth: u32,
    /// 结果回调：子 agent 完成后通知外部
    result_callback: Option<Arc<dyn SubAgentResultCallback>>,
}

impl SubAgentRunner {
    pub fn new(
        ai_manager: AiManager,
        tool_registry: Arc<ToolRegistry>,
        prompt_manager: Arc<FilePromptManager>,
        config: ChatConfig,
        depth: u32,
        max_depth: u32,
        result_callback: Option<Arc<dyn SubAgentResultCallback>>,
    ) -> Self {
        Self {
            ai_manager,
            tool_registry,
            prompt_manager,
            config,
            depth,
            max_depth,
            result_callback,
        }
    }
}

#[async_trait]
impl SubAgentSessionRunner for SubAgentRunner {
    async fn start(
        &self,
        arguments: Value,
        stream: ToolStreamSender,
    ) -> Result<SubAgentRunOutcome> {
        info!(
            "[子agent] start() 被调用, depth={}, max_depth={}",
            self.depth, self.max_depth
        );

        // 防递归
        if self.depth >= self.max_depth {
            info!("[子agent] 嵌套深度超限，拒绝执行");
            return Ok(SubAgentRunOutcome::Done(ToolResult {
                call_id: String::new(),
                is_error: true,
                content: Value::String(format!(
                    "子 agent 嵌套深度 {} 已达上限 {}",
                    self.depth, self.max_depth
                )),
            }));
        }

        // 提取 task 参数
        let task = serde_json::to_string_pretty(&arguments)
            .unwrap_or_else(|_| "请完成指定任务".to_string());
        info!("[子agent] 准备发送任务: {}", task);

        // 每次调用新建独立 ChatService：run_id 在构造时写入 config，
        // 完成后 service 自动 drop，driver loop 随 Arc<State> 归零自然退出，
        // history / subscribers / config 天然隔离。
        let mut call_config = self.config.clone();
        call_config.run_id = Some(stream.invocation_id().to_string());
        let service = ChatService::new(
            self.ai_manager.clone(),
            self.tool_registry.clone(),
            self.prompt_manager.clone(),
            call_config,
        )
        .map_err(|e| {
            info!("[子agent] ChatService::new 失败: {}", e);
            e
        })?;
        service.start_driver()?;

        // 发送任务给子 agent 的 ChatService
        let ticket = service.send_text(task).map_err(|e| {
            info!("[子agent] send_text 失败: {}", e);
            anyhow::anyhow!("{}", e)
        })?;
        info!("[子agent] send_text 成功，ticket 已返回，开始收集事件");

        // 收集事件直到完成或挂起
        // - Completed/Failed：service 随函数返回后 drop
        // - Suspended：service clone 移入 ChatSubAgentSession 持有
        collect_until_outcome(
            &service,
            ticket,
            &stream,
            self.depth,
            self.max_depth,
            self.result_callback.clone(),
        )
        .await
    }
}
