//! 灵活模式计划执行 Agent
//!
//! 与周密模式的 `PlanAndExecuteAgent` 不同，灵活模式：
//! - 不生成计划（计划已由灵活模式生成阶段产出）
//! - 步骤是**建议路径**而非强制路线，AI 可跳过/替换/重排
//! - 不保存新版本、不产出总结
//! - 事后通过 TraceRecorder 记录执行轨迹（用于离线固化分析）
//!
//! 核心流程：
//! ```
//! CoarseGrainedPlan + previous_summary + params
//!   → 渲染 flexible_execute_system.toml
//!   → chat_with_callback 自由执行
//!   → TraceRecorder 后台记录
//!   → 返回结果
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use serde_json::Value;
use tracing::{info, warn};

use planned_agent_core::planner::coarse::{CoarseGrainedPlan, CoarseGrainedStep};
use planned_agent_core::planner::react::{Action, Observation, ReActStep, Thought};
use planned_agent_core::prompt::{PromptContext, PromptManager};
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use crate::chat::{ChatEvent, ChatService};
use crate::planner::trace::TraceRecorder;

/// 灵活模式计划执行结果
#[derive(Debug, Clone)]
pub struct FlexiblePlanResult {
    /// 整体是否成功（未取消、未超 tool 调用上限）
    pub success: bool,
    /// 最后一条 assistant 消息
    pub final_message: Message,
    /// 完整消息历史
    pub history: Vec<Message>,
    /// 工具调用次数
    pub tool_calls_executed: usize,
    /// 执行耗时（毫秒）
    pub duration_ms: u64,
}

/// 灵活模式计划执行 Agent
///
/// 接收已生成的 [`CoarseGrainedPlan`]，通过 [`ChatService::chat_with_callback`]
/// 自由执行，事后异步记录 TraceRecorder 轨迹。
pub struct FlexiblePlanAgent<PM: PromptManager + Send + Sync + 'static> {
    chat_service: ChatService<PM>,
    prompt_manager: Arc<PM>,
    trace_recorder: Arc<TraceRecorder<PM>>,
}

impl<PM: PromptManager + Send + Sync + 'static> FlexiblePlanAgent<PM> {
    /// 创建新的 FlexiblePlanAgent。
    ///
    /// `chat_service` 应为 `system_prompt_template = None` 的实例——
    /// system prompt 由本 Agent 自行渲染并预置到 messages 首条，
    /// 避免 `ChatService` 二次注入。
    pub fn new(
        chat_service: ChatService<PM>,
        prompt_manager: Arc<PM>,
        trace_recorder: Arc<TraceRecorder<PM>>,
    ) -> Self {
        Self {
            chat_service,
            prompt_manager,
            trace_recorder,
        }
    }

    /// 执行灵活模式计划。
    ///
    /// # 参数
    /// - `plan`: 已生成的粗粒度计划（来自 DB 的 todos）
    /// - `previous_summary`: 上次执行经验总结（来自 DB 的 previous_summary）
    /// - `params`: 本次参数值（key → value）
    /// - `on_event`: 事件回调，实时转发给 GUI
    pub async fn execute<F>(
        &self,
        plan: &CoarseGrainedPlan,
        previous_summary: Option<&str>,
        params: &HashMap<String, String>,
        mut on_event: F,
    ) -> Result<FlexiblePlanResult>
    where
        F: FnMut(ChatEvent) + Send,
    {
        let start = Instant::now();

        // ── 1. 序列化步骤 ──
        let steps_text = Self::format_steps(&plan.steps);

        // ── 2. 序列化参数 ──
        let params_text = Self::format_params(params);

        // ── 3. 渲染 system prompt ──
        let summary_text = previous_summary.unwrap_or("（首次执行，无历史经验）");
        let system_prompt = self
            .prompt_manager
            .render(
                "planning/flexible_execute_system",
                &PromptContext::new()
                    .with_variable("steps", Value::String(steps_text))
                    .with_variable("previous_summary", Value::String(summary_text.to_string()))
                    .with_variable("params", Value::String(params_text)),
            )
            .await
            .map_err(|e| anyhow::anyhow!("渲染 flexible_execute_system 失败: {}", e))?;

        // ── 4. 构建 messages（System 已预置，ChatService 不会再注入）──
        let messages = vec![
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
                    text: format!("执行计划：{}", plan.title),
                }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            },
        ];

        // ── 5. 执行 ──
        let response = self
            .chat_service
            .chat_with_callback(messages, |event| {
                on_event(event);
            })
            .await?;

        let duration_ms = start.elapsed().as_millis() as u64;

        // ── 6. 后台记录 TraceRecorder（spawn 异步，不阻塞返回）──
        let recorder = self.trace_recorder.clone();
        let history = response.history.clone();
        let plan_title = plan.title.clone();
        tokio::spawn(async move {
            if let Err(e) =
                recorder.record_from_chat_history(&plan_title, &history, duration_ms).await
            {
                warn!("[FlexiblePlanAgent] TraceRecorder 记录失败: {}", e);
            }
        });

        info!(
            "[FlexiblePlanAgent] 执行完成: success={}, tools={}, {}ms",
            !response.cancelled,
            response.tool_calls_executed,
            duration_ms,
        );

        Ok(FlexiblePlanResult {
            success: !response.cancelled,
            final_message: response.message,
            history: response.history,
            tool_calls_executed: response.tool_calls_executed,
            duration_ms,
        })
    }

    // ── 辅助方法 ────────────────────────────────────────

    /// 将步骤列表格式化为 LLM 可读的文本
    fn format_steps(steps: &[CoarseGrainedStep]) -> String {
        if steps.is_empty() {
            return "（无参考步骤）".to_string();
        }
        steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let deps = if s.dependencies.is_empty() {
                    String::new()
                } else {
                    format!(" (依赖: {})", s.dependencies.join(", "))
                };
                format!(
                    "{}. [{}] {}\n   期望产出: {}{}",
                    i + 1,
                    s.result_reference,
                    s.intent,
                    s.expected_output,
                    deps,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 将参数 HashMap 格式化为文本
    fn format_params(params: &HashMap<String, String>) -> String {
        if params.is_empty() {
            return "（无预设参数）".to_string();
        }
        params
            .iter()
            .map(|(k, v)| format!("- {}: {}", k, v))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ─────────────────────────────────────────────────────────────────
// 辅助：从 Message 历史提取 ReActStep 序列（供 TraceRecorder 使用）
// ─────────────────────────────────────────────────────────────────

/// 从 chat_with_callback 产生的消息历史中提取 ReActStep 序列。
///
/// 匹配模式：`Assistant(tool_calls)` → `Tool(result)` 对。
pub(crate) fn extract_react_steps_from_messages(messages: &[Message]) -> Vec<ReActStep> {
    let mut steps: Vec<ReActStep> = Vec::new();
    // 收集所有 tool_call id → Action 映射
    let mut pending: HashMap<String, Action> = HashMap::new();

    for msg in messages {
        // 从 assistant 消息收集 tool_calls
        if let Some(tool_calls) = &msg.tool_calls {
            for tc in tool_calls {
                let params: Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                pending.insert(
                    tc.id.clone(),
                    Action {
                        tool_name: tc.function.name.clone(),
                        parameters: params,
                        reasoning: None,
                        tool_call_id: Some(tc.id.clone()),
                    },
                );
            }
        }

        // 从 tool 消息匹配对应的 action
        if matches!(msg.role, MessageRole::Tool) {
            if let Some(call_id) = &msg.tool_call_id {
                if let Some(action) = pending.remove(call_id) {
                    let raw_output = match &msg.content {
                        Some(MessageContent::ToolResult { content, .. }) => {
                            Value::String(content.clone())
                        }
                        _ => Value::Null,
                    };
                    let output = serde_json::from_str(
                        raw_output.as_str().unwrap_or("null"),
                    )
                    .unwrap_or_else(|_| raw_output.clone());

                    let is_error = output
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    steps.push(ReActStep {
                        thought: Thought {
                            reasoning: String::new(),
                            plan: String::new(),
                            confidence: 0.5,
                        },
                        action,
                        observation: Observation {
                            output: output.clone(),
                            raw_output,
                            is_complete: !is_error,
                            error: if is_error {
                                output
                                    .get("content")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                            } else {
                                None
                            },
                            duration_ms: 0,
                        },
                    });
                }
            }
        }
    }

    steps
}
