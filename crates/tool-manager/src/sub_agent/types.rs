//! 子 agent 接口定义（纯类型 + trait，零业务逻辑）。
//!
//! - [`SubAgentRunOutcome`]：单次执行的结果——完成或挂起等待用户输入；
//! - [`SubAgentSession`]：挂起时保留的会话状态，用户确认后经 `resume` 恢复；
//! - [`SubAgentSessionRunner`]：首次启动子 agent 的入口。

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use planned_agent_core::mcp::types::ToolResult;

use crate::sub_agent::stream::ToolStreamSender;

/// 子 agent 单次执行的结果
pub enum SubAgentRunOutcome {
    /// 完成：框架清理会话，`ToolResult` 走既有闭环
    Done(ToolResult),
    /// 挂起等待用户输入：`session` 交给框架保管（history 不丢），
    /// `message`/`actions` 用于渲染用户交互视图
    AwaitingUserAction {
        session: Box<dyn SubAgentSession>,
        message: String,
        actions: Value,
    },
}

/// 子 agent 会话：框架只负责存储与生命周期，状态内容由实现方定义。
///
/// 实现方在挂起时把内部状态（如 LLM 对话 history）装进 `Box<dyn SubAgentSession>`
/// 交给框架；`resume` 时框架把用户选择注入，实现方用保留的 history 继续执行。
#[async_trait]
pub trait SubAgentSession: Send + Sync {
    /// 恢复执行。可能再次返回 `AwaitingUserAction`（继续挂起，框架更新会话）。
    async fn resume(
        &mut self,
        user_input: Value,
        stream: ToolStreamSender,
    ) -> Result<SubAgentRunOutcome>;
}

/// 子 agent 会话式执行体（统一注册入口的 runner 类型）
#[async_trait]
pub trait SubAgentSessionRunner: Send + Sync {
    /// 首次启动。返回 `AwaitingUserAction` 时框架生成 `session_id` 并保管会话。
    async fn start(&self, arguments: Value, stream: ToolStreamSender)
        -> Result<SubAgentRunOutcome>;
}
