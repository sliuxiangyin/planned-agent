//! 子 agent 会话式执行抽象。
//!
//! 子 agent 以"会话"为单位执行，支持挂起-恢复：
//! - [`SubAgentSessionRunner`]：首次启动（`start`）；
//! - [`SubAgentRunOutcome`]：单次执行的结果——完成（`Done`）或
//!   挂起等待用户输入（`AwaitingUserAction`，内部状态交给框架保管）；
//! - [`SubAgentSession`]：挂起时保留的子 agent 内部状态（含其 history），
//!   用户确认后经 `resume` 恢复执行；
//! - 框架（[`crate::session::SubAgentSessionStore`]）负责会话的生命周期：
//!   挂起保留 → resume 时取出 → 完成/取消/TTL 清理。
//!
//! 过程事件经 [`ToolStreamSender`] 走旁路通道实时送达调用方（UI），
//! 最终结果走 `ToolResult → history → LLM` 闭环。
//! 挂起信息通过 `ToolResult.content` 结构化传递（不走流）：
//! `{"status":"awaiting_user_action","session_id":...,"message":...,"actions":[...]}`。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use planned_agent_core::mcp::types::ToolResult;
use planned_agent_core::tool_registry::ToolExecutor;

use crate::stream::ToolStreamSender;

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
    async fn start(
        &self,
        arguments: Value,
        stream: ToolStreamSender,
    ) -> Result<SubAgentRunOutcome>;
}

/// 一次性子 agent 适配器：把"单次执行闭包"包装成会话式 runner。
///
/// 适用于无需用户交互的子 agent：`start` 直接执行并返回 `Done`，
/// 永远不挂起。这样注册入口统一为 [`SubAgentSessionRunner`]。
pub struct OneShotSubAgentRunner {
    execute_fn:
        Arc<dyn Fn(Value, ToolStreamSender) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send>> + Send + Sync>,
}

impl OneShotSubAgentRunner {
    /// 用异步闭包创建一次性 runner
    pub fn new<F, Fut>(execute_fn: F) -> Self
    where
        F: Fn(Value, ToolStreamSender) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<ToolResult>> + Send + 'static,
    {
        let execute_fn = Arc::new(move |args: Value, stream: ToolStreamSender| {
            Box::pin(execute_fn(args, stream))
                as Pin<Box<dyn Future<Output = Result<ToolResult>> + Send + 'static>>
        });
        Self { execute_fn }
    }
}

#[async_trait]
impl SubAgentSessionRunner for OneShotSubAgentRunner {
    async fn start(
        &self,
        arguments: Value,
        stream: ToolStreamSender,
    ) -> Result<SubAgentRunOutcome> {
        let result = (self.execute_fn)(arguments, stream).await?;
        Ok(SubAgentRunOutcome::Done(result))
    }
}

/// 子 agent 执行器：实现 [`ToolExecutor`]，内部包装 [`SubAgentSessionRunner`]。
///
/// - 流式入口（`call_tool_streamed`）：
///   - 参数含 `session_id` → 从会话存储 **take** 出会话（防重入）→ `resume`；
///   - 否则 → `start`；
///   - `Done` → 会话已取出/未创建，正常返回，`call_id` 覆写为 `invocation_id`；
///   - `AwaitingUserAction` → 会话入存储（新会话生成 `session_id`，恢复的会话
///     沿用原 id 更新），返回结构化挂起 `ToolResult`；
/// - 非流式入口（`call_tool`）：传入 no-op 流句柄，行为与普通工具一致
///   （挂起时同样返回结构化 content，调用方自行解析）。
pub struct SubAgentToolExecutor {
    tool_name: String,
    runner: Arc<dyn SubAgentSessionRunner>,
    store: Arc<crate::session::SubAgentSessionStore>,
}

impl SubAgentToolExecutor {
    /// 创建子 agent 执行器（`tool_name` 为注册的工具名）
    pub fn new(
        tool_name: impl Into<String>,
        runner: Arc<dyn SubAgentSessionRunner>,
        store: Arc<crate::session::SubAgentSessionStore>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            runner,
            store,
        }
    }

    /// 流式执行入口：阻塞到子 agent 真正完成（含 resume 后继续）。
    ///
    /// `invocation_id` 同时用于流事件、最终 `ToolResult.call_id`、以及挂起会话的
    /// 存储 key（即 run_id）。子 agent 挂起（`AwaitingUserAction`）时，会话连同
    /// resume 信号存入 store，本方法阻塞等待前端 `signal_resume` 唤醒，随后取出
    /// 会话继续 `resume`，循环直到 `Done`。
    pub async fn execute_streamed(
        &self,
        arguments: Value,
        invocation_id: &str,
        stream: ToolStreamSender,
    ) -> Result<ToolResult> {
        // 首次 start（前端驱动 resume，不再依赖 LLM 带 session_id 重调工具）
        let mut outcome = self.runner.start(arguments, stream.clone()).await?;

        loop {
            match outcome {
                SubAgentRunOutcome::Done(mut result) => {
                    result.call_id = invocation_id.to_string();
                    return Ok(result);
                }
                SubAgentRunOutcome::AwaitingUserAction { session, .. } => {
                    // 挂起：会话 + resume 信号入 store（key = invocation_id = run_id）
                    let sid = invocation_id.to_string();
                    let (tx, rx) = oneshot::channel();
                    self.store
                        .upsert(sid.clone(), session, self.tool_name.clone(), tx);

                    // 阻塞等前端 resume 信号（用户选择）
                    let user_input = rx
                        .await
                        .map_err(|_| anyhow!("子 agent resume 通道关闭（可能已取消/超时）"))?;

                    // 取出会话（防重入），继续执行；可能再次挂起，循环
                    let mut session = self.store.take(&sid)?;
                    outcome = session.resume(user_input, stream.clone()).await?;
                }
            }
        }
    }
}

#[async_trait]
impl ToolExecutor for SubAgentToolExecutor {
    async fn execute(&self, _tool_name: &str, arguments: Value) -> Result<ToolResult> {
        // 非流式调用：保持「LLM 驱动 resume」的旧语义——挂起即返回结构化
        // `awaiting_user_action` ToolResult（session_id 由 LLM 下次重调工具带回），
        // 不阻塞等前端信号（非流式场景无前端 resume 入口）。
        let resume_sid: Option<String> = arguments
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let outcome = match &resume_sid {
            Some(sid) => {
                let mut session = self.store.take(sid)?;
                let user_input = arguments
                    .get("user_input")
                    .cloned()
                    .unwrap_or(Value::Null);
                session.resume(user_input, ToolStreamSender::disabled()).await
            }
            None => self.runner.start(arguments, ToolStreamSender::disabled()).await,
        };

        match outcome? {
            SubAgentRunOutcome::Done(mut result) => {
                result.call_id = uuid::Uuid::new_v4().to_string();
                Ok(result)
            }
            SubAgentRunOutcome::AwaitingUserAction {
                session,
                message,
                actions,
            } => {
                let sid = resume_sid.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                // 非流式无前端 resume，resume_tx 的 rx 立即 drop（signal 时 send 失败无害）
                let (tx, _rx) = oneshot::channel();
                self.store
                    .upsert(sid.clone(), session, self.tool_name.clone(), tx);
                Ok(ToolResult {
                    call_id: uuid::Uuid::new_v4().to_string(),
                    content: json!({
                        "status": "awaiting_user_action",
                        "session_id": sid,
                        "message": message,
                        "actions": actions,
                    }),
                    is_error: false,
                })
            }
        }
    }

    fn name(&self) -> &str {
        "sub_agent"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec![self.tool_name.clone()]
    }
}
