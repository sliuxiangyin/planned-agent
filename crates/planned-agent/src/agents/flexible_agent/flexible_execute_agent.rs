//! 灵活执行 Agent —— 纯执行器。
//!
//! 以**独立对话**（子 agent）运行：接收完整度文档，使用全量工具执行任务，
//! 返回压缩后的 [`ExecutionOutput`]（不含工具调用详情），
//! 主 Agent 将压缩结果注入全局上下文。
//!
//! ## 与 FlexiblePlanAgent 的区别
//!
//! `FlexiblePlanAgent` 接收 `CoarseGrainedPlan`（周密模式已生成的计划），
//! 面向周密模式的"按计划执行"。本 Agent 接收 `CompletenessDoc`
//! （灵活模式流程的中间状态），不依赖预生成计划。

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tracing::info;

use planned_agent_core::prompt::{PromptContext, PromptManager};
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use crate::chat::{ChatEvent, ChatService, SubAgentChatEvent};
use super::types::{CompletenessDoc, ExecutionOutput};

/// 灵活执行 Agent。
///
/// `chat_service` 应为 `system_prompt_template = None` 的实例——
/// system prompt 由本 Agent 自行渲染并预置到 messages[0]。
#[derive(Clone)]
pub struct FlexibleExecuteAgent<PM: PromptManager + Send + Sync + 'static> {
    chat_service: ChatService<PM>,
    prompt_manager: Arc<PM>,
}

impl<PM: PromptManager + Send + Sync + 'static> FlexibleExecuteAgent<PM> {
    /// 创建 FlexibleExecuteAgent。
    pub fn new(
        chat_service: ChatService<PM>,
        prompt_manager: Arc<PM>,
    ) -> Self {
        Self {
            chat_service,
            prompt_manager,
        }
    }

    /// 执行任务。
    ///
    /// # 参数
    /// - `doc`: 完整度文档（需求描述 + 可选：输入参数/执行步骤/工具路径/输出格式）
    /// - `on_event`: 事件回调，实时转发工具调用/执行事件给 GUI
    ///
    /// # 返回
    /// - `ExecutionOutput`: 压缩后的执行结果
    pub async fn execute<F>(
        &self,
        doc: &CompletenessDoc,
        mut on_event: F,
    ) -> Result<ExecutionOutput>
    where
        F: FnMut(ChatEvent) + Send,
    {
        let start = Instant::now();

        // 1. 渲染 system prompt
        let system_prompt = self
            .prompt_manager
            .render(
                "flexible/flexible_execute",
                &PromptContext::new(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("渲染 flexible_execute 失败: {}", e))?;

        // 2. 构建执行输入（完整度文档 Markdown + 历史参考）
        let mut user_input = format!(
            "## 需求描述\n{}\n",
            if doc.requirement.is_empty() { "（未提供）" } else { &doc.requirement }
        );

        if !doc.input_params.is_empty() {
            user_input.push_str(&format!("\n## 输入参数\n{}\n", doc.input_params));
        }
        if !doc.execution_steps.is_empty() {
            user_input.push_str(&format!(
                "\n## 历史执行步骤（供参考）\n{}\n",
                doc.execution_steps
            ));
        }
        if !doc.output_schema.is_empty() {
            user_input.push_str(&format!(
                "\n## 期望输出格式\n{}\n",
                doc.output_schema
            ));
        }

        user_input.push_str("\n请执行以上任务。");

        // 3. 构建 messages（System 预置，ChatService 不再注入）
        let messages = vec![
            Message {
                role: MessageRole::System,
                content: Some(MessageContent::Text { text: system_prompt }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                ..Default::default()
            },
            Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text { text: user_input }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                ..Default::default()
            },
        ];

        // 4. 执行（全量工具）
        // TODO(重构 service.rs): with_allowed_tools 语义可能调整，调用方需同步。
        let svc = self.chat_service.with_allowed_tools(None);
        // TODO(重构 service.rs): chat_with_callback 签名/返回可能变化（详见 batch_id + HistoryStore 重构方案），调用方需同步。
        let response = svc.chat_with_callback(messages, |event| {
            on_event(event);
        }, None::<fn(SubAgentChatEvent)>).await?;

        let duration_ms = start.elapsed().as_millis() as u64;

        if response.cancelled {
            return Err(anyhow::anyhow!("执行被用户取消"));
        }

        // 5. 解析最终结果
        let final_text = extract_last_assistant_text(&response.history);
        let output = parse_execution_output(&final_text).map_err(|e| {
            anyhow::anyhow!("执行结果 JSON 解析失败: {}（原始: {}）", e, &final_text[..120.min(final_text.len())])
        })?;

        info!(
            "[FlexibleExecuteAgent] 执行完成: tools={}, {}ms, key_steps={}",
            response.tool_calls_executed,
            duration_ms,
            output.key_steps.len(),
        );

        Ok(output)
    }
}

// ── 辅助 ──

fn extract_last_assistant_text(history: &[Message]) -> String {
    history
        .iter()
        .rev()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .and_then(|m| match &m.content {
            Some(MessageContent::Text { text }) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn parse_execution_output(text: &str) -> Result<ExecutionOutput, String> {
    let json_str = text
        .trim()
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .map(|s| s.strip_suffix("```").unwrap_or(s))
        .unwrap_or(text)
        .trim();
    serde_json::from_str::<ExecutionOutput>(json_str)
        .map_err(|e| format!("JSON 反序列化失败: {}", e))
}
