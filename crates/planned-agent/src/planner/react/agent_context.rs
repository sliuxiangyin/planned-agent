//! Agent 消息上下文管理
//!
//! 负责 ReAct Agent 的对话历史维护、Prompt 上下文构建、意图注入。
//! 将消息列表和意图缓存从 DefaultReActAgent 中独立出来，
//! 保持消息操作的单一职责。

use serde_json::Value;

use planned_agent_core::planner::coarse::CoarseGrainedStep;
use planned_agent_core::planner::react::Observation;
use planned_agent_core::prompt::PromptContext;
use planned_agent_core::types::{Message, MessageContent, MessageRole};

use super::intent_handler::IntentHandler;
use super::intent_router::IntentRouter;

/// 消息上下文管理器
///
/// 持有对话消息列表和意图缓存，封装消息构建与 Push 逻辑。
pub(crate) struct AgentContext {
    /// 对话消息列表
    messages: Vec<Message>,
    /// 缓存的 intent handler 结果（避免 observe 时重复路由）
    cached_intent_vars: Vec<(&'static str, Value)>,
}

impl AgentContext {
    /// 创建空的 AgentContext
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            cached_intent_vars: Vec::new(),
        }
    }

    /// 获取当前消息列表的不可变引用
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    // ── 工具函数 ──────────────────────────────────────────

    /// 人类可读的数据大小格式化
    pub fn format_size(bytes: usize) -> String {
        if bytes >= 1024 * 1024 {
            format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
        } else if bytes >= 1024 {
            format!("{:.1}KB", bytes as f64 / 1024.0)
        } else {
            format!("{}B", bytes)
        }
    }

    // ── Prompt 上下文构建 ─────────────────────────────────

    /// 向 ReAct Prompt 注入完整步骤约束。
    pub fn with_step_context(
        prompt_context: PromptContext,
        coarse_step: &CoarseGrainedStep,
    ) -> PromptContext {
        let step_value = serde_json::json!({
            "intent": coarse_step.intent,
            "expected_output": coarse_step.expected_output,
        });
        let data_requirements = serde_json::to_string_pretty(&coarse_step.data_requirements)
            .unwrap_or_else(|_| "[]".to_string());

        prompt_context
            .with_variable("coarse_step", step_value)
            .with_variable("data_requirements", serde_json::json!(data_requirements))
    }

    /// 构建后续步骤摘要字符串（用于 think prompt）
    pub fn build_remaining_steps_str(steps: Option<&[CoarseGrainedStep]>) -> String {
        match steps {
            Some(steps) if !steps.is_empty() => steps
                .iter()
                .map(|s| format!("- 步骤{} ({}): {}", s.order, s.result_reference, s.intent))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => "无".to_string(),
        }
    }

    /// 根据 CoarseGrainedStep 解析主导意图，合并至 PromptContext。
    ///
    /// 首次调用时路由并缓存结果，后续调用直接复用缓存。
    pub fn with_intent_flags(
        &mut self,
        coarse_step: &CoarseGrainedStep,
        mut ctx: PromptContext,
    ) -> PromptContext {
        if self.cached_intent_vars.is_empty() {
            let intents = IntentRouter::route(coarse_step);
            self.cached_intent_vars = IntentHandler::handle(intents)
                .into_iter()
                .map(|(k, v)| (k, v))
                .collect();
        }
        for (k, v) in &self.cached_intent_vars {
            ctx = ctx.with_variable(k.to_string(), v.clone());
        }
        ctx
    }

    // ── 消息初始化 ────────────────────────────────────────

    /// 初始化消息列表：System + User。
    ///
    /// 调用方负责渲染 System prompt 并传入。
    pub fn init_messages(&mut self, system_prompt: String, user_message: String) {
        self.messages = vec![
            Message {
                role: MessageRole::System,
                content: Some(MessageContent::Text {
                    text: system_prompt,
                }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            },
            Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text {
                    text: user_message,
                }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            },
        ];
    }

    // ── 消息 Push ─────────────────────────────────────────

    /// 推送 User 消息到 messages
    pub fn push_user_message(&mut self, text: String) {
        self.messages.push(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text }),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        });
    }

    /// 推送带 tool_calls 的 Assistant 消息到 messages
    pub fn push_assistant_message_raw(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    /// 推送工具结果到 messages。
    pub fn push_tool_result_message(
        &mut self,
        tool_call_id: String,
        observation: &Observation,
    ) {
        let tool_result_str = serde_json::to_string_pretty(&observation.output)
            .unwrap_or_else(|_| serde_json::to_string(&observation.output).unwrap_or_default());

        self.messages.push(Message {
            role: MessageRole::Tool,
            content: Some(MessageContent::ToolResult {
                tool_call_id: tool_call_id.clone(),
                content: tool_result_str,
            }),
            tool_calls: None,
            tool_call_id: Some(tool_call_id),
            name: None,
            reasoning_content: None,
        });
    }
}

impl Default for AgentContext {
    fn default() -> Self {
        Self::new()
    }
}
