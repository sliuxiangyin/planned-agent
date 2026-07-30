//! 默认 ReAct Agent 实现
//!
//! 核心职责：
//! - 维护 Agent 生命周期（配置、依赖注入）
//! - ReAct 主循环（think+act → execute → observe）
//! - 委托给子模块：消息管理（agent_context）、工具执行（tool_executor）、引用展开（ref_expander）

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use std::fs;
use std::sync::Arc;
use std::time::Instant;
use tracing::{error, info, warn};

use planned_agent_core::ai::AiClient;
use planned_agent_core::planner::coarse::CoarseGrainedStep;
use planned_agent_core::planner::react::*;
use planned_agent_core::prompt::{PromptContext, PromptManager};
use planned_agent_core::types::{Message, MessageContent, PlanContext};
use planned_agent_tool_manager::ToolRegistry;

use super::agent_context::AgentContext;
use super::chunk::ChunkStore;
use super::chunk::executor_context::ExecutorContext;
use super::ref_expander::expand_refs;
use super::step_store::StepStore;
use super::tool_executor;

/// 默认 ReAct Agent 实现
pub struct DefaultReActAgent<PM: PromptManager> {
    /// AI 客户端
    ai_client: Arc<dyn AiClient>,
    /// 提示管理器
    prompt_manager: Arc<PM>,
    /// 工具注册表
    tool_registry: Arc<ToolRegistry>,
    /// 配置
    config: ReActAgentConfig,
    /// 消息上下文（对话历史 + 意图缓存）
    ctx: AgentContext,
    /// 步骤结果存储（由 PlanAndExecuteAgent 传入）
    store: Option<StepStore>,
    /// 分片缓存（工具输出大文本自动分片）
    chunk_store: Arc<ChunkStore>,
}

impl<PM: PromptManager + 'static> DefaultReActAgent<PM> {
    /// 创建新的 DefaultReActAgent
    pub fn new(
        ai_client: Arc<dyn AiClient>,
        prompt_manager: Arc<PM>,
        tool_registry: Arc<ToolRegistry>,
        exec_ctx: Arc<ExecutorContext>,
        config: ReActAgentConfig,
    ) -> Self {
        let chunk_store = Arc::new(ChunkStore::new());
        exec_ctx.set_chunk_store(chunk_store.clone());

        Self {
            ai_client,
            prompt_manager,
            tool_registry: tool_registry.clone(),
            config,
            ctx: AgentContext::new(),
            store: None,
            chunk_store,
        }
    }

    /// 设置步骤结果存储（由 PlanAndExecuteAgent 调用）
    pub fn set_store(&mut self, store: StepStore) {
        self.store = Some(store);
    }

    /// 保存完整对话历史到 logs/ 目录
    fn save_messages_to_log(step_id: &str, result: &ReActExecutionResult, messages: &[Message]) {
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S");
        let filename = format!("logs/messages_{}_{}.json", step_id, timestamp);

        // 确保目录存在
        if let Err(e) = fs::create_dir_all("logs") {
            warn!("无法创建 logs 目录: {}", e);
            return;
        }

        let record = serde_json::json!({
            "step_id": step_id,
            "timestamp": timestamp.to_string(),
            "success": result.success,
            "iterations": result.iterations,
            "duration_ms": result.total_duration_ms,
            "observe_summary": result.observe_summary,
            "error": result.error,
            "messages": messages,
        });

        match serde_json::to_string_pretty(&record) {
            Ok(json) => {
                if let Err(e) = fs::write(&filename, &json) {
                    warn!("保存 messages 到 {} 失败: {}", filename, e);
                } else {
                    info!("[SAVE] 对话历史已保存 → {} ({}条消息)", filename, messages.len());
                }
            }
            Err(e) => {
                warn!("序列化 messages 失败: {}", e);
            }
        }
    }

    // ── 消息初始化 ────────────────────────────────────────

    /// 初始化消息列表：渲染 System prompt 后委托 AgentContext 构建消息
    async fn init_messages(
        &mut self,
        coarse_step: &CoarseGrainedStep,
        context: &PlanContext,
        remaining_steps: Option<&[CoarseGrainedStep]>,
    ) -> Result<()> {
        // 获取用户目标
        let user_goal = context
            .metadata
            .get("user_goal")
            .and_then(|v| v.as_str())
            .unwrap_or("无")
            .to_string();

        // 构建工具名称列表（文本形式注入 System prompt）
        let step_categories = coarse_step
            .recommended_tool_categories
            .as_deref()
            .unwrap_or(&[]);
        let tool_names =
            tool_executor::get_tools_description(&self.tool_registry, step_categories);

        // 构建后续步骤摘要
        let remaining_steps_str = AgentContext::build_remaining_steps_str(remaining_steps);

        // 读取前序步骤结果（从共享 store），生成摘要避免系统提示膨胀
        let previous_results_str = match &self.store {
            Some(store) => {
                let entries = store.list_entries().unwrap_or_default();
                if entries.is_empty() {
                    "（暂无前序步骤结果）".to_string()
                } else {
                    let mut lines: Vec<String> = Vec::new();
                    for e in &entries {
                        if e.is_error {
                            lines.push(format!("  - {}: ❌ 失败 — 此步骤无可用数据", e.ref_id));
                        } else {
                            let size = AgentContext::format_size(e.data_size);
                            let summary = e.summary.as_deref().unwrap_or("[摘要待补充]");
                            lines.push(format!(
                                "  - {}: ✅ {} — 执行摘要：{}",
                                e.ref_id, size, summary
                            ));
                        }
                    }
                    lines.join("\n")
                }
            }
            None => "（无步骤结果存储）".to_string(),
        };

        // 动态渲染 System prompt（注入 step 上下文 + 工具名称 + intent_hints）
        let prompt_context = self.ctx.with_intent_flags(
            coarse_step,
            AgentContext::with_step_context(
                PromptContext::new()
                    .with_variable("remaining_steps", serde_json::json!(remaining_steps_str))
                    .with_variable("tool_names", serde_json::json!(tool_names))
                    .with_variable("previous_results", serde_json::json!(previous_results_str)),
                coarse_step,
            ),
        );
        let system_prompt = self
            .prompt_manager
            .render("planning/react_system", &prompt_context)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to render template planning/react_system: {}", e)
            })?;

        self.ctx
            .init_messages(system_prompt, format!("用户目标：\n{}", user_goal));

        Ok(())
    }

    // ── think+act 合并 ────────────────────────────────────

    /// think+act 合并：LLM 自主输出 tool_calls 或 DONE
    ///
    /// 返回 (actions, assistant_msg)。空 Vec 表示 DONE。
    /// 支持多个 tool_call——全部处理，不再丢弃。
    /// assistant_msg 需要由调用方 push 到 messages。
    async fn think_and_act(
        &mut self,
        coarse_step: &CoarseGrainedStep,
    ) -> Result<(Vec<Action>, Message)> {
        // 按 step 构建 tools 定义（含类别工具 + 引用工具）
        let tools = tool_executor::build_tool_definitions(&self.tool_registry, coarse_step);

        // 调用 LLM（带 tools），LLM 自主决策：输出 tool_calls 或回复 DONE
        let assistant_msg =
            tool_executor::call_llm_with_messages(&self.ai_client, self.ctx.messages(), tools)
                .await?;

        // LLM 返回内容日志
        info!(
            "[LLM] content={:?} tool_calls={:?}",
            assistant_msg
                .content
                .as_ref()
                .and_then(|c| match c {
                    planned_agent_core::types::MessageContent::Text { text } => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .unwrap_or("(no text)"),
            assistant_msg
                .tool_calls
                .as_ref()
                .map(|v| v.iter().map(|tc| &tc.function.name).collect::<Vec<_>>())
        );

        // 检查是否声明 DONE（无 tool_calls 且 content 以 DONE 开头）
        if assistant_msg.tool_calls.is_none()
            || assistant_msg
                .tool_calls
                .as_ref()
                .map(|v| v.is_empty())
                .unwrap_or(true)
        {
            if let Ok(text) = tool_executor::extract_text_content(&assistant_msg) {
                if text.trim().to_lowercase().starts_with("done") {
                    return Ok((vec![], assistant_msg));
                }
            }
        }

        // 从 tool_calls 提取全部 Action
        let tool_calls = match assistant_msg.tool_calls.as_ref() {
            Some(tc) if !tc.is_empty() => tc,
            _ => {
                warn!(
                    "LLM returned no tool_calls and no DONE marker. Treating as completion. Content: {:?}",
                    assistant_msg
                        .content
                        .as_ref()
                        .and_then(|c| match c {
                            planned_agent_core::types::MessageContent::Text { text } => {
                                Some(text.chars().take(200).collect::<String>())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| "(non-text)".to_string())
                );
                return Ok((vec![], assistant_msg));
            }
        };

        if tool_calls.len() > 1 {
            info!(
                "LLM returned {} tool_calls, processing all sequentially.",
                tool_calls.len()
            );
        }

        let actions: Vec<Action> = tool_calls
            .iter()
            .map(|tc| {
                let parameters: Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                Action {
                    tool_name: tc.function.name.clone(),
                    parameters,
                    reasoning: None,
                    tool_call_id: Some(tc.id.clone()),
                }
            })
            .collect();

        Ok((actions, assistant_msg))
    }
}

#[async_trait]
impl<PM: PromptManager + 'static> ReActAgent for DefaultReActAgent<PM> {
    async fn execute_coarse_step(
        &mut self,
        coarse_step: &CoarseGrainedStep,
        context: &PlanContext,
    ) -> Result<ReActExecutionResult> {
        let start_time = Instant::now();
        let mut history = Vec::new();

        // 从 context 中获取后续步骤信息
        let remaining_steps: Option<Vec<CoarseGrainedStep>> = context
            .metadata
            .get("remaining_steps")
            .and_then(|v| serde_json::from_value(v.clone()).ok());

        // 初始化消息列表
        self.init_messages(coarse_step, context, remaining_steps.as_deref())
            .await?;

        // 循环检测状态
        let mut last_action: Option<String> = None;
        let mut repeat_count: u32 = 0;
        let max_repeats = 3;

        let result = {
            let mut iteration = 0;
            'outer: loop {
                // ── 迭代上限检查 ──
                if iteration >= self.config.max_iterations {
                    break Ok(ReActExecutionResult::failure(
                        coarse_step.id.clone(),
                        format!("Exceeded max iterations: {}", self.config.max_iterations),
                        history,
                        self.config.max_iterations,
                        start_time.elapsed().as_millis() as u64,
                    ));
                }

                // ── 超时检查 ──
                if start_time.elapsed().as_millis() as u64 > self.config.step_timeout_ms {
                    warn!(
                        "Step timeout after {}ms (limit: {}ms)",
                        start_time.elapsed().as_millis(),
                        self.config.step_timeout_ms
                    );
                    break Ok(ReActExecutionResult::failure(
                        coarse_step.id.clone(),
                        format!(
                            "Step timeout: {}ms exceeded (elapsed: {}ms)",
                            self.config.step_timeout_ms,
                            start_time.elapsed().as_millis()
                        ),
                        history,
                        iteration,
                        start_time.elapsed().as_millis() as u64,
                    ));
                }

                // 当前 / 下一步意图（用于后处理路由）
                let next_intent = remaining_steps
                    .as_ref()
                    .and_then(|steps| steps.first().map(|step| step.intent.clone()))
                    .unwrap_or_default();
                let current_intent = coarse_step.intent.clone();

                // ── 1. THINK+ACT ──
                let (actions, assistant_msg) = self.think_and_act(coarse_step).await?;
                // 迭代日志：当前轮次、执行动作、LLM 文本回复
                info!(
                    "[STEP:{}] [ITER#{}] tools={:?} content={:?}",
                    coarse_step.id,
                    iteration,
                    actions.iter().map(|a| &a.tool_name).collect::<Vec<_>>(),
                    assistant_msg.content.as_ref().and_then(|c| match c {
                        MessageContent::Text { text } => Some(text.as_str()),
                        _ => None,
                    }).unwrap_or("(none)")
                );

                // ── 2. 检查 DONE ──
                if actions.is_empty() {
                    let done_text: String =
                        tool_executor::extract_text_content(&assistant_msg).unwrap_or_default();
                    self.ctx.push_assistant_message_raw(assistant_msg);
                    break Ok(ReActExecutionResult::success(
                        coarse_step.id.clone(),
                        serde_json::json!({"status": "done", "content": done_text}),
                        None,  // DONE 场景无 observe 摘要
                        history,
                        iteration + 1,
                        start_time.elapsed().as_millis() as u64,
                    ));
                }

                // 推入 Assistant 消息（含全部 tool_calls）
                self.ctx.push_assistant_message_raw(assistant_msg);

                // ── 3. 顺序执行所有 action ──
                let mut last_observation: Option<Observation> = None;
                for action in &actions {
                    // 循环检测：连续相同 action → 终止
                    {
                        let action_sig = format!("{}:{}", action.tool_name, action.parameters);
                        if Some(&action_sig) == last_action.as_ref() {
                            repeat_count += 1;
                            if repeat_count >= max_repeats {
                                error!(
                                    "Detected {} repeats of same action, breaking loop",
                                    repeat_count
                                );
                                break 'outer Ok(ReActExecutionResult::failure(
                                    coarse_step.id.clone(),
                                    format!(
                                        "Repeated same action {} times: {}",
                                        repeat_count, action.tool_name
                                    ),
                                    history,
                                    iteration + 1,
                                    start_time.elapsed().as_millis() as u64,
                                ));
                            }
                        } else {
                            last_action = Some(action_sig);
                            repeat_count = 0;
                        }
                    }

                    // EXECUTE - 调用工具
                    let observation = match self
                        .execute_tool(action, &current_intent, &next_intent)
                        .await
                    {
                        Ok(obs) => obs,
                        Err(e) => {
                            error!("Execute tool failed: {}", e);
                            Observation {
                                output: Value::Null,
                                is_complete: false,
                                error: Some(e.to_string()),
                                duration_ms: 0,
                            }
                        }
                    };

                    // 将工具结果作为 Tool 消息追加（分片视图自动压缩）
                    let tool_call_id = action
                        .tool_call_id
                        .clone()
                        .unwrap_or_else(|| "tool_result".to_string());
                    self.ctx
                        .push_tool_result_message(tool_call_id, &observation);

                    // 记录历史
                    history.push(ReActStep {
                        thought: Thought {
                            reasoning: String::new(),
                            plan: String::new(),
                            confidence: 0.5,
                        },
                        action: action.clone(),
                        observation: observation.clone(),
                    });

                    last_observation = Some(observation);
                }

                // ── 4. OBSERVE（范围守卫：每次 ACT 后检查是否越界）──
                let final_obs = last_observation.unwrap_or(Observation {
                    output: Value::Null,
                    is_complete: false,
                    error: Some("No actions executed".to_string()),
                    duration_ms: 0,
                });

                let observe_result = match self.observe(coarse_step, &final_obs).await {
                    Ok(result) => result,
                    Err(e) => {
                        error!("Observe failed: {}", e);
                        ObserveResult::default()
                    }
                };

                info!("[OBSERVE] complete={} out_of_scope={} summary={:?}",
                    observe_result.is_complete, observe_result.is_out_of_scope, observe_result.summary);

                if let Some(last) = history.last_mut() {
                    last.thought.reasoning = observe_result.summary.clone();
                    last.thought.confidence = if observe_result.is_complete { 1.0 } else { 0.5 };
                }

                if observe_result.is_out_of_scope {
                    let scope_warning = format!(
                        "⚠️ 你当前的行为超出了本步骤的范围。\n当前步骤只需要: {}\n请严格只完成这一步，完成后输出 DONE。不要做后续步骤的事情。",
                        coarse_step.expected_output
                    );
                    self.ctx.push_user_message(scope_warning);
                    warn!("[SCOPE] step={} out-of-scope detected: {}",
                        coarse_step.id, observe_result.summary);
                }

                // ── 5. observe 完成处理 ──
                if observe_result.is_complete {
                    let summary = Some(observe_result.summary.clone());
                    break Ok(ReActExecutionResult::success(
                        coarse_step.id.clone(),
                        final_obs.output.clone(),
                        summary,
                        history,
                        iteration + 1,
                        start_time.elapsed().as_millis() as u64,
                    ));
                }

                iteration += 1;
            }
        };

        // 保存完整对话历史到 logs/
        if let Ok(ref exec_result) = result {
            Self::save_messages_to_log(&coarse_step.id, exec_result, self.ctx.messages());
        }

        result
    }

    async fn execute_tool(
        &self,
        action: &Action,
        _current_intent: &str,
        _next_intent: &str,
    ) -> Result<Observation> {
        let start_time = Instant::now();
        let tool_name = action.tool_name.clone();
        let mut parameters = action.parameters.clone();
        let call_id = action.tool_call_id.as_deref().unwrap_or("unknown");

        // 工具调用日志
        info!(
            "[TOOL] name={} params={}",
            tool_name,
            serde_json::to_string(&parameters).unwrap_or_default()
        );

        // ── 路由到对应处理器 ──
        match tool_name.as_str() {
            "ai_process" => {
                // 先展开引用再调用 AI 子流程
                if let Some(ref store) = self.store {
                    let guard = store
                        .read()
                        .map_err(|e| anyhow::anyhow!("StepStore 读锁失败: {}", e))?;
                    expand_refs(&mut parameters, &guard);
                }
                tool_executor::handle_ai_process(&self.ai_client, &parameters, start_time).await
            }
            _ => {
                // expand_refs：对标识符类工具（reference 用于查找，非数据展开）跳过
                if tool_name != "builtin_fetch_step_result" {
                    if let Some(ref store) = self.store {
                        let guard = store
                            .read()
                            .map_err(|e| anyhow::anyhow!("StepStore 读锁失败: {}", e))?;
                        expand_refs(&mut parameters, &guard);
                    }
                }
                tool_executor::handle_generic_tool(
                    &self.tool_registry,
                    &self.chunk_store,
                    &self.store,
                    &tool_name,
                    parameters,
                    call_id,
                    start_time,
                )
                .await
            }
        }
    }

    async fn observe(
        &mut self,
        coarse_step: &CoarseGrainedStep,
        observation: &Observation,
    ) -> Result<ObserveResult> {
        // 渲染 observe prompt
        let tool_result_str = if observation.error.is_some() {
            format!("工具执行错误: {}", observation.error.as_ref().unwrap())
        } else {
            serde_json::to_string_pretty(&observation.output)
                .unwrap_or_else(|_| serde_json::to_string(&observation.output).unwrap_or_default())
        };

        let prompt_context = self.ctx.with_intent_flags(
            coarse_step,
            PromptContext::new()
                .with_variable(
                    "coarse_step",
                    serde_json::json!({
                        "intent": coarse_step.intent,
                        "expected_output": coarse_step.expected_output,
                    }),
                )
                .with_variable("tool_result", serde_json::json!(tool_result_str)),
        );

        let prompt = self
            .prompt_manager
            .render("planning/react_observe", &prompt_context)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to render template planning/react_observe: {}", e)
            })?;

        // 用独立调用判断完成度和范围（避免 messages 历史中的 tool_calls 模式污染 LLM 输出）
        let response = tool_executor::call_llm(&self.ai_client, &prompt).await?;

        // 解析响应
        #[derive(serde::Deserialize)]
        struct ObserveResponse {
            is_complete: bool,
            reasoning: String,
            #[serde(default)]
            is_out_of_scope: bool,
        }

        let observe_response: ObserveResponse = self
            .prompt_manager
            .parse_response("planning/react_observe", &response)
            .await?;

        // observe 结论回流到 messages（下轮 think+act 可见上下文）
        self.ctx.push_user_message(prompt);
        self.ctx.push_assistant_message_raw(Message {
            role: planned_agent_core::types::MessageRole::Assistant,
            content: Some(planned_agent_core::types::MessageContent::Text {
                text: response,
            }),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        });

        Ok(ObserveResult {
            is_complete: observe_response.is_complete,
            summary: observe_response.reasoning,
            is_out_of_scope: observe_response.is_out_of_scope,
        })
    }

    fn name(&self) -> &str {
        "DefaultReActAgent"
    }
}
