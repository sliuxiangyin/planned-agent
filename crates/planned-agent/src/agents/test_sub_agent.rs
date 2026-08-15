//! 真实 LLM 子 agent：内部复用 [`ChatService`] 跑完整 agent 循环
//! （与 GUI flexible 页面同一套机制，支持过程流转发、工具调用、挂起-恢复）。
//!
//! - `start`：把子任务作为 user 消息交给内部 `ChatService` 执行；
//!   内部 `ChatEvent` 经同步转发到主旁路（`ToolStreamSender`）；
//! - 内部遇到 `request_user_action`（`pending_ui_actions` 非空）→ 挂起
//!   `AwaitingUserAction`，会话保存内部 history 与 `ChatService` 副本；
//! - `resume`：把用户选择作为新 user 消息追加，继续内部对话直至完成。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_ai_manager::AiManager;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_prompt_manager::FilePromptManager;
use planned_agent_tool_manager::{
    SubAgentRunOutcome, SubAgentSession, SubAgentSessionRunner, ToolRegistry, ToolStreamSender,
};

use crate::chat::{ChatConfig, ChatEvent, ChatService, SubAgentChatEvent};

/// 真实 LLM 子 agent runner：内部持有一个 `ChatService`。
// TODO(重构 service.rs): ChatService 类型签名/泛型可能变化，调用方需同步。
pub struct ChatSubAgent {
    inner: ChatService<FilePromptManager>,
}

impl ChatSubAgent {
    /// 创建真实 LLM 子 agent（内部独立 `cancelled` 标志）
    ///
    /// - `registry`：子 agent 可用的工具注册表（通常与主 agent 共享，
    ///   包含 `request_user_action` 时可挂起等待用户输入，包含本工具时可嵌套）；
    /// - `config`：子 agent 的 `ChatConfig`（`system_prompt_template` / `allowed_tools` 等）。
    pub fn new(
        ai_manager: AiManager,
        registry: Arc<ToolRegistry>,
        prompt_manager: Arc<FilePromptManager>,
        config: ChatConfig,
    ) -> Self {
        Self {
            inner: ChatService::new(ai_manager, registry, prompt_manager, config),
        }
    }

    /// 创建真实 LLM 子 agent（共享外部 `cancelled` 标志）
    ///
    /// 主 agent 把自己的 `Arc<AtomicBool>` 传进来 → 主 agent 调用 `stop()` 时
    /// 子 agent 内部 `chat_with_callback` 的 `is_cancelled()` 检查会立即命中，
    /// 子 agent 内部循环立刻退出。否则子 agent 完全感知不到外部取消信号，
    /// 会一直跑到 LLM 自然结束或挂起等待。
    pub fn new_with_cancelled(
        ai_manager: AiManager,
        registry: Arc<ToolRegistry>,
        prompt_manager: Arc<FilePromptManager>,
        config: ChatConfig,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        // TODO(重构 service.rs): ChatService::new 构造签名可能变化（详见 service.rs 重构方案），调用方需同步。
        let inner = ChatService::new(ai_manager, registry, prompt_manager, config)
            // TODO(重构 service.rs): with_cancelled 语义可能调整，调用方需同步。
            .with_cancelled(cancelled);
        Self { inner }
    }
}

#[async_trait]
impl SubAgentSessionRunner for ChatSubAgent {
    async fn start(
        &self,
        arguments: Value,
        stream: ToolStreamSender,
    ) -> anyhow::Result<SubAgentRunOutcome> {
        stream.status_sync("started");
        let task = arguments["task"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                serde_json::to_string_pretty(&arguments)
                    .unwrap_or_else(|_| arguments.to_string())
            });
        // 提示词模板由 ChatConfig.system_prompt_template 指定（默认灵活：演示 action 类型）。
        // 这里补一段开场 user 消息让子 agent 知道本轮要做什么：
        // - 默认调用场景：开头先调用 request_user_action 以演示 confirm。
        let hint = arguments["hint"]
            .as_str()
            .unwrap_or(
                "请作为子 agent 独立完成以下子任务，完成后用简洁中文给出结论。\
                 如需用户确认/选择/补充信息，按 prompt 中定义的四步流程逐步演示。",
            );
        let history = vec![user_message(format!("{}\n\n子任务：{}", hint, task))];
        run_sub_agent_round(&self.inner, history, &stream).await
    }
}

/// 挂起时保留的子 agent 会话（内部 history + ChatService 副本）
// TODO(重构 service.rs): ChatService 类型签名/泛型可能变化，调用方需同步。
struct ChatSubAgentSession {
    inner: ChatService<FilePromptManager>,
    history: Vec<Message>,
}

#[async_trait]
impl SubAgentSession for ChatSubAgentSession {
    async fn resume(
        &mut self,
        user_input: Value,
        stream: ToolStreamSender,
    ) -> anyhow::Result<SubAgentRunOutcome> {
        let mut history = std::mem::take(&mut self.history);
        history.push(user_message(format!("用户确认/选择：{}", user_input)));
        run_sub_agent_round(&self.inner, history, &stream).await
    }
}

/// 内部执行一轮：跑完整个 agent 循环（可多轮工具调用），
/// 直到完成（返回最终结论）或请求用户输入（挂起）。
async fn run_sub_agent_round(
    // TODO(重构 service.rs): ChatService 类型签名/泛型可能变化，调用方需同步。
    inner: &ChatService<FilePromptManager>,
    history: Vec<Message>,
    stream: &ToolStreamSender,
) -> anyhow::Result<SubAgentRunOutcome> {
    // TODO(重构 service.rs): chat_with_callback 签名/返回可能变化（详见 batch_id + HistoryStore 重构方案），调用方需同步。
    let response = inner
        .chat_with_callback(
            history,
            |ev| forward_sub_agent_event(ev, stream),
            Some(|out: SubAgentChatEvent| {
                // 嵌套子 agent 的输出转发到主旁路（原样携带原始 ChatEvent）
                stream.emit_event_sync(out.event);
            }),
        )
        .await?;

    // 内部请求用户输入 → 挂起（history 保留，resume 后继续）
    if let Some(pending) = response.pending_ui_actions.first() {
        let actions = serde_json::to_value(&pending.actions)
            .unwrap_or_else(|_| Value::Array(vec![]));
        return Ok(SubAgentRunOutcome::AwaitingUserAction {
            session: Box::new(ChatSubAgentSession {
                inner: inner.clone(),
                history: response.history,
            }),
            message: pending.message.clone(),
            actions,
        });
    }

    // 完成：提取最后一个 assistant 文本作为结论
    let text = extract_last_assistant_text(&response.history);
    stream.summary_sync("子 agent 完成");
    Ok(SubAgentRunOutcome::Done(ToolResult {
        call_id: String::new(), // 执行器覆写为 invocation_id
        content: json!({ "result": text }),
        is_error: false,
    }))
}

/// 内部 `ChatEvent` → 主旁路（同步转发，`FnMut` 回调内不可 await）。
///
/// 全量类型化转发：不再降级为字符串事件，`RoundStart`/`RoundEnd`/
/// `ToolCallArgsDelta`/`UIActionRequest` 等此前被丢弃的事件也原样送达；
/// `ReasoningDelta` 不再混入 `TextDelta`。
fn forward_sub_agent_event(ev: ChatEvent, stream: &ToolStreamSender) {
    stream.emit_event_sync(ev);
}

/// 从 history 提取最后一个非空 assistant 文本
fn extract_last_assistant_text(history: &[Message]) -> String {
    history
        .iter()
        .rev()
        .find_map(|m| {
            if matches!(m.role, MessageRole::Assistant) {
                if let Some(MessageContent::Text { text }) = &m.content {
                    if !text.is_empty() {
                        return Some(text.clone());
                    }
                }
            }
            None
        })
        .unwrap_or_default()
}

fn user_message(text: String) -> Message {
    Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text { text }),
        ..Default::default()
    }
}

/// 构造子 agent 工具定义（注册时框架自动注入 `session_id` / `user_input` 可选字段）
pub fn test_sub_agent_tool() -> Tool {
    Tool {
        name: "test_sub_agent".to_string(),
        description: "真实 LLM 子 agent：把子任务独立交给一个子 agent 完成（内部有自己的对话循环，\
                      按 system prompt（默认 chat/request_user_action_demo）行为，\
                      可调用其他工具；需要用户确认时会挂起等待，确认后继续并返回最终结论）。\
                      适合需要独立思考、多步处理或并行推进的子任务。"
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "交给子 agent 的子任务描述（自然语言）"
                },
                "hint": {
                    "type": "string",
                    "description": "可选：附加的 user 提示（追加在 task 前，作为本轮子 agent 的开场指令）"
                }
            },
            "required": ["task"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_definition_is_valid() {
        let tool = test_sub_agent_tool();
        assert_eq!(tool.name, "test_sub_agent");
        assert_eq!(tool.input_schema["required"][0], "task");
        // 框架注入的会话字段应存在
        let props = &tool.input_schema["properties"];
        assert!(props.get("task").is_some());
    }

    #[test]
    fn extract_last_assistant_text_prefers_latest_non_empty() {
        let history = vec![
            Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text {
                    text: "任务".to_string(),
                }),
                ..Default::default()
            },
            Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Text {
                    text: "第一轮结论".to_string(),
                }),
                ..Default::default()
            },
            // 工具结果消息（非 assistant，应跳过）
            Message {
                role: MessageRole::Tool,
                content: Some(MessageContent::Text {
                    text: "tool output".to_string(),
                }),
                ..Default::default()
            },
            Message {
                role: MessageRole::Assistant,
                content: Some(MessageContent::Text {
                    text: "最终结论".to_string(),
                }),
                ..Default::default()
            },
        ];
        assert_eq!(extract_last_assistant_text(&history), "最终结论");
    }

    #[test]
    fn extract_last_assistant_text_empty_when_no_text() {
        let history = vec![Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text {
                text: "任务".to_string(),
            }),
            ..Default::default()
        }];
        assert_eq!(extract_last_assistant_text(&history), "");
    }

    #[test]
    fn user_message_creates_user_role_text() {
        let m = user_message("hello".to_string());
        assert!(matches!(m.role, MessageRole::User));
        assert!(matches!(
            &m.content,
            Some(MessageContent::Text { text }) if text == "hello"
        ));
    }
}
