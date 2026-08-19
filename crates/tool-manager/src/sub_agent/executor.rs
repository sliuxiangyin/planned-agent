//! 子 agent 会话式执行实现。
//!
//! 接口定义见 [`super::types`]，本模块只包含业务逻辑：
//! - [`OneShotSubAgentRunner`]：一次性子 agent 适配器（不挂起）；
//! - [`SubAgentToolExecutor`]：会话式执行器（挂起-恢复循环）；
//! - [`resume_loop`]：共享的多次挂起-恢复循环。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use planned_agent_core::mcp::types::ToolResult;
use planned_agent_core::tool_registry::ToolExecutor;

use super::types::{SubAgentRunOutcome, SubAgentSession, SubAgentSessionRunner};
use crate::sub_agent::stream::ToolStreamSender;

/// 一次性子 agent 适配器：把"单次执行闭包"包装成会话式 runner。
///
/// 适用于无需用户交互的子 agent：`start` 直接执行并返回 `Done`，
/// 永远不挂起。这样注册入口统一为 [`SubAgentSessionRunner`]。
pub struct OneShotSubAgentRunner {
    execute_fn: Arc<
        dyn Fn(Value, ToolStreamSender) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send>>
            + Send
            + Sync,
    >,
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

/// 通用 resume 循环：resume → 如果又挂起就放回 store → 等下一次用户操作 → 再 resume
/// 直到子 agent 完成（Done）或出错。由 `execute_streamed` 内部调用。
async fn resume_loop(
    mut session: Box<dyn SubAgentSession>,
    user_input: Value,
    store: Arc<crate::sub_agent::session::SubAgentSessionStore>,
    sid: String,
    tool_name: String,
    stream: ToolStreamSender,
) -> Result<SubAgentRunOutcome> {
    let mut outcome = session.resume(user_input, stream.clone()).await;
    loop {
        match outcome {
            Err(e) => return Err(e),
            Ok(SubAgentRunOutcome::Done(result)) => {
                return Ok(SubAgentRunOutcome::Done(result));
            }
            Ok(SubAgentRunOutcome::AwaitingUserAction {
                session: next_session,
                ..
            }) => {
                // 子 agent 又挂起了 → 放回 store → 等下一次用户操作
                let (tx, rx) = oneshot::channel();
                store.upsert(sid.clone(), next_session, tool_name.clone(), tx);
                let inp = rx
                    .await
                    .map_err(|_| anyhow!("子 agent resume 通道关闭（可能已取消/超时）"))?;
                let mut s = store.take(&sid)?;
                outcome = s.resume(inp, stream.clone()).await;
            }
        }
    }
}

/// 子 agent 执行器：实现 [`ToolExecutor`]，内部包装 [`SubAgentSessionRunner`]。
///
/// - 流式入口（`call_tool_streamed`）：调用 `start`，若子 agent 挂起则阻塞等待
///   前端 `signal_resume` 唤醒，通过 `resume_loop` 处理所有后续轮次，直到完成。
/// - 非流式入口（`call_tool`）：传入 no-op 流句柄，行为与普通工具一致
///   （挂起时返回结构化 content，调用方自行解析）。
pub struct SubAgentToolExecutor {
    tool_name: String,
    runner: Arc<dyn SubAgentSessionRunner>,
    store: Arc<crate::sub_agent::session::SubAgentSessionStore>,
}

impl SubAgentToolExecutor {
    /// 创建子 agent 执行器（`tool_name` 为注册的工具名）
    pub fn new(
        tool_name: impl Into<String>,
        runner: Arc<dyn SubAgentSessionRunner>,
        store: Arc<crate::sub_agent::session::SubAgentSessionStore>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            runner,
            store,
        }
    }

    /// 流式执行入口：阻塞到子 agent 真正完成。
    ///
    /// `invocation_id` 同时用于流事件、最终 `ToolResult.call_id`、以及挂起会话的
    /// 存储 key（即 run_id）。子 agent 挂起（`AwaitingUserAction`）时，会话存入
    /// store 并阻塞等待前端 `signal_resume` 唤醒，随后通过 `resume_loop` 处理
    /// 所有后续轮次（多次挂起-恢复），直到子 agent 完成后返回最终 `ToolResult`。
    pub async fn execute_streamed(
        &self,
        arguments: Value,
        invocation_id: &str,
        stream: ToolStreamSender,
    ) -> Result<ToolResult> {
        // 首次调用路径：start 并返回结构化挂起或完成结果
        let outcome = self.runner.start(arguments, stream.clone()).await?;
        match outcome {
            SubAgentRunOutcome::Done(mut result) => {
                result.call_id = invocation_id.to_string();
                Ok(result)
            }
            SubAgentRunOutcome::AwaitingUserAction {
                session,
                ..
            } => {
                // 首次挂起：存入 store（带 resume signal），然后阻塞等待完成。
                //
                // 不再 spawn 后台 task + 立即返回 awaiting_user_action。
                // 而是：存 session → 等用户输入 → resume_loop 直到 Done → 返回最终结果。
                // 这样父 agent 的 history 里 tool result 是子 agent 的最终输出，
                // 而非中间挂起状态，父 LLM 不会误判。
                let sid = invocation_id.to_string();
                let (tx, rx) = oneshot::channel();
                self.store
                    .upsert(sid.clone(), session, self.tool_name.clone(), tx);

                // 等前端 signal_resume 发送用户输入
                let user_input = rx
                    .await
                    .map_err(|_| anyhow!("子 agent resume 通道关闭（可能已取消/超时）"))?;

                // 取出会话，进入 resume_loop 阻塞直到子 agent 真正完成
                let session = self.store.take(&sid)?;
                let outcome = resume_loop(
                    session,
                    user_input,
                    self.store.clone(),
                    sid,
                    self.tool_name.clone(),
                    stream,
                )
                .await?;

                match outcome {
                    SubAgentRunOutcome::Done(mut result) => {
                        result.call_id = invocation_id.to_string();
                        Ok(result)
                    }
                    _ => unreachable!("resume_loop should only return Done or Err"),
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
                let user_input = arguments.get("user_input").cloned().unwrap_or(Value::Null);
                session
                    .resume(user_input, ToolStreamSender::disabled())
                    .await
            }
            None => {
                self.runner
                    .start(arguments, ToolStreamSender::disabled())
                    .await
            }
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
