//! v2_chat 子 agent：基于 `V2ChatService` 实现 `SubAgentSessionRunner` / `SubAgentSession`。
//!
//! 核心思路：**不重复实现 ReAct 循环**，直接复用 `V2ChatService::send_text`，
//! 子 agent 与父 agent 共享同一个 `ToolRegistry`，但拥有独立的 prompt 和配置。
//!
//! 挂起-恢复采用业界 run_id 模型：
//! - `run_id` = 父 agent 调用本子 agent 时的 `invocation_id`（tool_call_id）；
//! - 挂起时子 agent 的 `run_conversation` 以 `EmitAndSuspend` 策略提前返回
//!   （`UIActionRequest` 已冒泡到前端），runner 返回 `AwaitingUserAction`；
//! - 恢复时前端经 `signal_resume` 唤醒 `execute_streamed`，取出 session 后
//!   `resume` 压入 tool 消息闭合协议，从 history **继续原对话**（不是新 send）。

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::events::{ChatEvent, UIAction};
use planned_agent_core::mcp::types::ToolResult;
use planned_agent_prompt_manager::FilePromptManager;
use planned_agent_tool_manager::{
    SubAgentRunOutcome, SubAgentSession, SubAgentSessionRunner, ToolStreamSender,
};
use serde_json::Value;
use tracing::info;

use crate::v2_chat::service::{SendOutcome, SendTicket, V2ChatEvent, V2ChatService};

// ── Runner ─────────────────────────────────────────────────────────────────

/// 子 agent runner：持有 `V2ChatService`，直接复用其 ReAct 循环。
///
/// - `depth`：当前嵌套深度（0 = 顶层）
/// - `max_depth`：最大允许嵌套深度（防递归）
pub struct V2ChatSubAgentRunner {
    service: V2ChatService<FilePromptManager>,
    depth: u32,
    max_depth: u32,
}

impl V2ChatSubAgentRunner {
    pub fn new(
        service: V2ChatService<FilePromptManager>,
        depth: u32,
        max_depth: u32,
    ) -> Self {
        Self { service, depth, max_depth }
    }
}

#[async_trait]
impl SubAgentSessionRunner for V2ChatSubAgentRunner {
    async fn start(
        &self,
        arguments: Value,
        stream: ToolStreamSender,
    ) -> Result<SubAgentRunOutcome> {
        info!("[子agent] start() 被调用, depth={}, max_depth={}", self.depth, self.max_depth);

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
        let task = arguments["task"]
            .as_str()
            .unwrap_or("请完成指定任务");
        info!("[子agent] 准备发送任务: {}", task);

        // 启动子 agent 的 driver（幂等，多次调用安全）
        self.service.start_driver()?;

        // 注入 run_id（invocation_id）：子 agent 挂起时 UIActionRequest 携带它，
        // 前端 resume 据此路由回本会话。子 agent 串行执行，复用 service 安全。
        self.service.set_run_id(Some(stream.invocation_id().to_string()));

        // 发送任务给子 agent 的 V2ChatService
        let ticket = self
            .service
            .send_text(task)
            .map_err(|e| {
                info!("[子agent] send_text 失败: {}", e);
                anyhow::anyhow!("{}", e)
            })?;
        info!("[子agent] send_text 成功，ticket 已返回，开始收集事件");

        // 收集事件直到完成或挂起
        collect_until_outcome(&self.service, ticket, &stream, self.depth, self.max_depth).await
    }
}

// ── Session ────────────────────────────────────────────────────────────────

/// 子 agent 会话：保存 `V2ChatService` 引用，支持 resume。
///
/// `V2ChatService` 内部维护 history（checkpoint），挂起时历史保留，
/// resume 时压入 tool 消息闭合协议后从历史继续。
pub struct V2ChatSubAgentSession {
    service: V2ChatService<FilePromptManager>,
    depth: u32,
    max_depth: u32,
}

impl V2ChatSubAgentSession {
    pub fn new(
        service: V2ChatService<FilePromptManager>,
        depth: u32,
        max_depth: u32,
    ) -> Self {
        Self { service, depth, max_depth }
    }
}

#[async_trait]
impl SubAgentSession for V2ChatSubAgentSession {
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

        // 继续原对话：压入 tool 消息闭合挂起的 request_user_action，
        // 然后从 history 继续 run_conversation（不是 send 新消息）。
        let ticket = self
            .service
            .resume(&choice, &action_id)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        collect_until_outcome(&self.service, ticket, &stream, self.depth, self.max_depth).await
    }
}

// ── 事件收集 ──────────────────────────────────────────────────────────────

/// 监听子 agent 的事件流，转发到 `ToolStreamSender`，
/// 直到对话完成（`Completed`）或挂起（`Suspended`）。
async fn collect_until_outcome(
    service: &V2ChatService<FilePromptManager>,
    ticket: SendTicket,
    stream: &ToolStreamSender,
    depth: u32,
    max_depth: u32,
) -> Result<SubAgentRunOutcome> {
    info!("[子agent] collect_until_outcome 开始，注册事件监听");

    // 克隆 stream 以便传入闭包（闭包需要 'static）
    let stream_clone = stream.clone();

    // 捕获挂起时 UIActionRequest 的 message / actions（用于构造 AwaitingUserAction）
    let ui_request: Arc<Mutex<Option<(String, Vec<UIAction>)>>> = Arc::new(Mutex::new(None));
    let ui_request_clone = ui_request.clone();

    // 注册临时事件监听：转发子 agent 内部事件，并捕获挂起 UI 信息
    let _guard = service.on_chat_with_guard(move |event| {
        if let V2ChatEvent::Chat(chat_event) = &event {
            if let ChatEvent::UIActionRequest { message, actions, .. } = chat_event {
                *ui_request_clone.lock().unwrap() = Some((message.clone(), actions.clone()));
            }
            // 转发流式事件给父 agent 的 stream（含 UIActionRequest，冒泡到前端）
            stream_clone.emit_event_sync(chat_event.clone());
        }
    });

    // 等待子 agent 对话结果（区分完成 / 挂起 / 失败）
    match ticket.wait_outcome().await {
        SendOutcome::Completed => {
            info!("[子agent] 子 agent 完成，提取结果");
            let history = service.history();
            let last_text = extract_last_assistant_text(&history);
            Ok(SubAgentRunOutcome::Done(ToolResult {
                call_id: String::new(),
                is_error: false,
                content: Value::String(last_text),
            }))
        }
        SendOutcome::Suspended { .. } => {
            info!("[子agent] 子 agent 挂起，构造 AwaitingUserAction");
            let (message, actions) = ui_request.lock().unwrap().take().unwrap_or_default();
            Ok(SubAgentRunOutcome::AwaitingUserAction {
                session: Box::new(V2ChatSubAgentSession::new(
                    service.clone(),
                    depth,
                    max_depth,
                )),
                message,
                actions: serde_json::to_value(actions).unwrap_or_else(|_| Value::Array(vec![])),
            })
        }
        SendOutcome::Failed(e) => {
            info!("[子agent] 子 agent 失败: {}", e);
            Ok(SubAgentRunOutcome::Done(ToolResult {
                call_id: String::new(),
                is_error: true,
                content: Value::String(format!("子 agent 执行失败: {}", e)),
            }))
        }
    }
}

/// 从历史中提取最后一条 assistant 文本消息
fn extract_last_assistant_text(history: &[Message]) -> String {
    for msg in history.iter().rev() {
        if matches!(msg.role, MessageRole::Assistant) {
            if let Some(MessageContent::Text { text }) = &msg.content {
                if !text.is_empty() {
                    return text.clone();
                }
            }
        }
    }
    String::new()
}
