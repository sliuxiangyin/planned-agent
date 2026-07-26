use std::sync::Arc;
use std::time::Instant;
use async_trait::async_trait;
use anyhow::{Result, anyhow};
use serde_json::Value;
use tracing::{debug, error, info};

use planned_agent_core::ai::AiClient;
use planned_agent_core::prompt::{PromptManager, PromptContext};
use planned_agent_core::planner::coarse::CoarseGrainedStep;
use planned_agent_core::planner::react::*;
use planned_agent_core::tool_registry::ToolCategory;
use planned_agent_tool_manager::ToolRegistry;
use planned_agent_core::types::{PlanContext, ChatCompletionRequest, Message, MessageRole, MessageContent};

use super::sub_agents::html_clean_subagent::HtmlCleanSubAgent;
use super::intent_handler::IntentHandler;
use super::intent_router::IntentRouter;
use super::tool_result_router::ToolResultRouter;

/// 默认 ReAct Agent 实现
pub struct DefaultReActAgent<PM: PromptManager> {
    /// AI 客户端
    ai_client: Arc<dyn AiClient>,
    /// 提示管理器
    prompt_manager: Arc<PM>,
    /// 工具注册表
    tool_registry: Arc<ToolRegistry>,
    /// 工具结果路由器：内部持有已注册的 handlers，一站式完成"路由 + 执行"
    tool_result_router: ToolResultRouter,
    /// 配置
    config: ReActAgentConfig,
}

impl<PM: PromptManager + 'static> DefaultReActAgent<PM> {
    /// 创建新的 DefaultReActAgent
    pub fn new(
        ai_client: Arc<dyn AiClient>,
        prompt_manager: Arc<PM>,
        tool_registry: Arc<ToolRegistry>,
        config: ReActAgentConfig,
    ) -> Self {
        let html_sub_agent = Arc::new(HtmlCleanSubAgent::with_llm_decider(
            ai_client.clone(),
            prompt_manager.clone(),
            tool_registry.clone(),
        ));
        // 标准两个 handler 直接由 router 内部封装注册
        let tool_result_router = ToolResultRouter::with_standard_handlers(html_sub_agent, 50_000);

        Self {
            ai_client,
            prompt_manager,
            tool_registry,
            tool_result_router,
            config,
        }
    }
    
    /// 使用默认配置创建
    pub fn with_defaults(
        ai_client: Arc<dyn AiClient>,
        prompt_manager: Arc<PM>,
        tool_registry: Arc<ToolRegistry>,
    ) -> Self {
        Self::new(
            ai_client,
            prompt_manager,
            tool_registry,
            ReActAgentConfig::default(),
        )
    }

    /// 向 ReAct Prompt 注入原始用户目标和完整步骤约束。
    fn with_step_context(
        prompt_context: PromptContext,
        coarse_step: &CoarseGrainedStep,
        context: &PlanContext,
    ) -> PromptContext {
        let user_goal = context
            .metadata
            .get("user_goal")
            .cloned()
            .unwrap_or_else(|| Value::String("无".to_string()));
        let step_value = serde_json::to_value(coarse_step).unwrap_or_else(|_| {
            serde_json::json!({
                "intent": coarse_step.intent,
                "expected_output": coarse_step.expected_output,
            })
        });
        let data_requirements = serde_json::to_string_pretty(&coarse_step.data_requirements)
            .unwrap_or_else(|_| "[]".to_string());

        prompt_context
            .with_variable("user_goal", user_goal)
            .with_variable("coarse_step", step_value)
            .with_variable("data_requirements", serde_json::json!(data_requirements))
    }

    /// 根据 CoarseGrainedStep 解析主导意图，并把模板可消费的 flags 合并进 PromptContext。
    ///
    /// 流程：先路由（IntentRouter::route）→ 再处理（IntentHandler::handle）。
    /// MixedFocus 时 `has_intent_hint=false`，模板中的 `{% if %}` 整体短路，
    /// 对 prompt 体积零影响。
    fn with_intent_flags(
        coarse_step: &CoarseGrainedStep,
        mut ctx: PromptContext,
    ) -> PromptContext {
        let intent = IntentRouter::route(coarse_step);
        debug!(target: "react_agent", "Resolved step intent: {:?} for step {}", intent, coarse_step.id);
        for (k, v) in IntentHandler::handle(intent) {
            ctx = ctx.with_variable(k.to_string(), v);
        }
        ctx
    }
    
    /// 调用 LLM
    async fn call_llm(&self, prompt: &str) -> Result<String> {
        let request = ChatCompletionRequest {
            model: self.ai_client.model_name().to_string(),
            messages: vec![Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text { text: prompt.to_string() }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            }],
            tools: None,
            temperature: Some(0.3),
            max_tokens: Some(2000),
            stream: false,
            extra: Default::default(),
        };
        
        let response = self.ai_client.chat_completion(request).await?;
        
        // 提取响应内容
        if let Some(choice) = response.choices.first() {
            if let Some(content) = &choice.message.content {
                match content {
                    MessageContent::Text { text } => Ok(text.clone()),
                    _ => Err(anyhow!("Unexpected response content type")),
                }
            } else {
                Err(anyhow!("No content in response"))
            }
        } else {
            Err(anyhow!("No choices in response"))
        }
    }
    
    /// 获取可用工具列表描述
    ///
    /// 按 `categories` 过滤工具；若为空（None 或 []），兜底返回所有启用工具。
    /// 这是修复 tool-prompt 噪声的关键点：让 LLM 只看到与步骤相关的工具。
    fn get_tools_description(&self, categories: &[ToolCategory]) -> String {
        let tools = if categories.is_empty() {
            self.tool_registry.get_all_tools()
        } else {
            self.tool_registry.get_tools_by_categories(categories)
        };
        let mut desc = String::new();

        for tool in &tools {
            desc.push_str(&format!(
                "- {}: {}\n  Schema：{}\n",
                tool.name,
                tool.description,
                serde_json::to_string(&tool.input_schema).unwrap_or_default()
            ));
        }

        desc
    }
    
    /// 执行AI处理工具
    pub async fn execute_ai_process(
        &self,
        parameters: &Value,
        start_time: Instant,
    ) -> Result<Observation> {
        let data = parameters.get("data").cloned().unwrap_or(Value::Null);
        let instruction = parameters.get("instruction")
            .and_then(|v| v.as_str())
            .unwrap_or("处理数据");
        
        // 构建prompt
        let data_str = serde_json::to_string_pretty(&data)
            .unwrap_or_else(|_| serde_json::to_string(&data).unwrap_or_default());
        
        let prompt = format!(
            "请根据以下指令处理数据：\n\n指令：{}\n\n数据：\n{}\n\n请直接返回处理结果，不要包含其他说明。",
            instruction, data_str
        );
        
        // 重试机制：最多重试3次
        let max_retries = 3;
        let mut last_error = None;
        
        for attempt in 0..max_retries {
            if attempt > 0 {
                debug!("AI处理重试第 {} 次", attempt);
            }
            
            // 调用AI处理
            let result = self.call_llm(&prompt).await;
            
            match result {
                Ok(response) => {
                    // 尝试解析为JSON
                    let output = serde_json::from_str::<Value>(&response)
                        .unwrap_or_else(|_| Value::String(response));
                    
                    let duration_ms = start_time.elapsed().as_millis() as u64;
                    return Ok(Observation {
                        output,
                        is_complete: false,
                        error: None,
                        duration_ms,
                    });
                }
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            }
        }
        
        // 所有重试都失败
        let duration_ms = start_time.elapsed().as_millis() as u64;
        Ok(Observation {
            output: Value::Null,
            is_complete: false,
            error: Some(format!("AI处理失败（重试{}次后）: {}", max_retries, last_error.unwrap())),
            duration_ms,
        })
    }
}

#[async_trait]
impl<PM: PromptManager + 'static> ReActAgent for DefaultReActAgent<PM> {
    async fn execute_coarse_step(
        &self,
        coarse_step: &CoarseGrainedStep,
        context: &PlanContext,
    ) -> Result<ReActExecutionResult> {
        let start_time = Instant::now();
        let mut history = Vec::new();
        
        info!("Starting ReAct execution for step: {}", coarse_step.intent);
        
        // 从 context 中获取后续步骤信息
        let remaining_steps: Option<Vec<CoarseGrainedStep>> = context.metadata
            .get("remaining_steps")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        
        for iteration in 0..self.config.max_iterations {
            debug!("ReAct iteration {}/{}", iteration + 1, self.config.max_iterations);

            // 计算下一步意图（用于后处理路由）
            let next_intent = remaining_steps
                .as_ref()
                .and_then(|steps| steps.first().map(|step| step.intent.clone()))
                .unwrap_or_default();
            let current_intent = coarse_step.intent.clone();

            // 1. THINK - 思考
            let thought = match self.think(coarse_step, &history, context, remaining_steps.as_deref()).await {
                Ok(thought) => {
                    debug!("Thought: {} (confidence: {})", thought.reasoning, thought.confidence);
                    thought
                }
                Err(e) => {
                    error!("Think failed: {}", e);
                    return Ok(ReActExecutionResult::failure(
                        coarse_step.id.clone(),
                        format!("Think failed: {}", e),
                        history,
                        iteration,
                        start_time.elapsed().as_millis() as u64,
                    ));
                }
            };

            // 2. ACT - 行动
            let mut act_result = None;
            for retry in 0..3 {
                match self.act(coarse_step, &thought, context).await {
                    Ok(action) => {
                        debug!("Action: {} with params {}", action.tool_name, action.parameters);
                        act_result = Some(action);
                        break;
                    }
                    Err(e) => {
                        error!("Act failed (retry {}): {}", retry + 1, e);
                        if retry == 2 {
                            return Ok(ReActExecutionResult::failure(
                                coarse_step.id.clone(),
                                format!("Act failed after 3 retries: {}", e),
                                history,
                                iteration,
                                start_time.elapsed().as_millis() as u64,
                            ));
                        }
                    }
                }
            }
            let action = act_result.unwrap();

            // 3. EXECUTE - 调用工具并经过后处理
            let observation = match self.execute_tool(&action, &current_intent, &next_intent).await {
                Ok(obs) => {
                    debug!("Observation: error={:?}", obs.error);
                    obs
                }
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

            // 4. OBSERVE - 分析工具执行结果，提取关键信息
            let observe_result = match self.observe(coarse_step, &observation).await {
                Ok(result) => {
                    debug!("Observe: complete={}, summary={}", result.is_complete, result.summary);
                    result
                }
                Err(e) => {
                    error!("Observe failed: {}", e);
                    ObserveResult {
                        is_complete: false,
                        summary: format!("Observe failed: {}", e),
                    }
                }
            };

            // 记录历史（清洗后的结果进入历史）
            history.push(ReActStep {
                thought: thought.clone(),
                action: action.clone(),
                observation: observation.clone(),
            });

            // 5. 判断是否完成
            if observe_result.is_complete {
                info!("ReAct execution completed in {} iterations", iteration + 1);
                return Ok(ReActExecutionResult::success(
                    coarse_step.id.clone(),
                    observation.output,
                    history,
                    iteration + 1,
                    start_time.elapsed().as_millis() as u64,
                ));
            }
        }
        
        error!("ReAct execution exceeded max iterations: {}", self.config.max_iterations);
        Ok(ReActExecutionResult::failure(
            coarse_step.id.clone(),
            format!("Exceeded max iterations: {}", self.config.max_iterations),
            history,
            self.config.max_iterations,
            start_time.elapsed().as_millis() as u64,
        ))
    }
    
    async fn think(
        &self,
        coarse_step: &CoarseGrainedStep,
        history: &[ReActStep],
        context: &PlanContext,
        remaining_steps: Option<&[CoarseGrainedStep]>,
    ) -> Result<Thought> {
        // 构建历史记录字符串
        let history_str = if history.is_empty() {
            "无".to_string()
        } else {
            history.iter().enumerate().map(|(i, step)| {
                let output_str = serde_json::to_string_pretty(&step.observation.output)
                    .unwrap_or_else(|_| serde_json::to_string(&step.observation.output).unwrap_or_default());
                format!(
                    "第{}轮：\n思考：{}\n行动：{}({})\n观察：{}",
                    i + 1,
                    step.thought.reasoning,
                    step.action.tool_name,
                    step.action.parameters,
                    output_str
                )
            }).collect::<Vec<_>>().join("\n\n")
        };
        
        // 获取前序步骤的原始输出
        let previous_outputs = context.metadata.get("previous_outputs")
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
            .unwrap_or_else(|| "无".to_string());
        
        // 构建后续步骤摘要
        let remaining_steps_str = if let Some(steps) = remaining_steps {
            if steps.is_empty() {
                "无".to_string()
            } else {
                steps.iter()
                    .map(|s| format!("- 步骤{}: {}", s.order, s.intent))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        } else {
            "无".to_string()
        };
        
        // 使用 prompt_manager 渲染 prompt
        let step_categories = coarse_step.recommended_tool_categories.as_deref().unwrap_or(&[]);
        let prompt_context = Self::with_intent_flags(
            coarse_step,
            Self::with_step_context(
                PromptContext::new()
                    .with_variable("tools", serde_json::json!(self.get_tools_description(step_categories)))
                    .with_variable("history", serde_json::json!(history_str))
                    .with_variable("previous_outputs", serde_json::json!(previous_outputs))
                    .with_variable("remaining_steps", serde_json::json!(remaining_steps_str)),
                coarse_step,
                context,
            ),
        );

        let prompt = self.prompt_manager.render("planning/react_think", &prompt_context).await
            .map_err(|e| anyhow::anyhow!("Failed to render template planning/react_think: {}", e))?;

        debug!(
            target: "react_prompt",
            "[planning/react_think] step={} chars={}\n{}",
            coarse_step.id,
            prompt.chars().count(),
            prompt
        );

        // 调用 LLM
        let response = self.call_llm(&prompt).await?;
        
        // 使用 prompt_manager 解析响应
        let thought: Thought = self.prompt_manager.parse_response("planning/react_think", &response).await?;
        
        Ok(thought)
    }
    
    async fn act(
        &self,
        coarse_step: &CoarseGrainedStep,
        thought: &Thought,
        context: &PlanContext,
    ) -> Result<Action> {
        // 获取前序步骤的原始输出
        let previous_outputs = context.metadata.get("previous_outputs")
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
            .unwrap_or_else(|| "无".to_string());
        
        // 使用 prompt_manager 渲染 prompt
        let step_categories = coarse_step.recommended_tool_categories.as_deref().unwrap_or(&[]);
        let prompt_context = Self::with_intent_flags(
            coarse_step,
            Self::with_step_context(
                PromptContext::new()
                    .with_variable("thought", serde_json::json!({
                        "reasoning": thought.reasoning,
                        "plan": thought.plan
                    }))
                    .with_variable("tools", serde_json::json!(self.get_tools_description(step_categories)))
                    .with_variable("previous_outputs", serde_json::json!(previous_outputs)),
                coarse_step,
                context,
            ),
        );

        let prompt = self.prompt_manager.render("planning/react_act", &prompt_context).await?;

        debug!(
            target: "react_prompt",
            "[planning/react_act] step={} chars={}\n{}",
            coarse_step.id,
            prompt.chars().count(),
            prompt
        );

        // 调用 LLM
        let response = self.call_llm(&prompt).await?;
        
        // 使用 prompt_manager 解析响应
        let action: Action = self.prompt_manager.parse_response("planning/react_act", &response).await?;
        
        Ok(action)
    }
    
    /// 调用工具 + 后处理（路由 → handler 链式执行）。
    ///
    /// `current_intent` / `next_intent` 用于 handler 内部决策（如 HTML 清洗子 Agent 的格式选择）。
    async fn execute_tool(
        &self,
        action: &Action,
        current_intent: &str,
        next_intent: &str,
    ) -> Result<Observation> {
        let start_time = Instant::now();
        let tool_name = action.tool_name.clone();
        let parameters = action.parameters.clone();

        // 特殊处理 ai_process 工具：不经后处理，直接走原 AI 流程
        if tool_name == "ai_process" {
            return self.execute_ai_process(&parameters, start_time).await;
        }

        let outcome_result = self
            .tool_registry
            .call_tool(&tool_name, parameters)
            .await;

        let duration_ms = start_time.elapsed().as_millis() as u64;

        let outcome = match outcome_result {
            Ok(o) => o,
            Err(e) => {
                return Ok(Observation {
                    output: Value::Null,
                    is_complete: false,
                    error: Some(e.to_string()),
                    duration_ms,
                });
            }
        };

        let raw_obs = Observation {
            output: outcome.result.content,
            is_complete: false,
            error: if outcome.result.is_error {
                Some("Tool returned error".to_string())
            } else {
                None
            },
            duration_ms,
        };

        // 在 execute_tool 内部一站式完成"路由 + handler 链式执行"。
        // 原始 HTML 永远不会写进主上下文。
        Ok(self
            .tool_result_router
            .process(raw_obs, &outcome.categories, current_intent, next_intent)
            .await)
    }
    
    /// 观察：分析工具执行结果，判断是否完成目标
    async fn observe(
        &self,
        coarse_step: &CoarseGrainedStep,
        observation: &Observation,
    ) -> Result<ObserveResult> {
        // 如果工具执行出错，直接返回
        if observation.error.is_some() {
            return Ok(ObserveResult {
                is_complete: false,
                summary: format!("工具执行错误: {}", observation.error.as_ref().unwrap()),
            });
        }
        
        // 使用 prompt_manager 渲染 prompt（使用 react_observe 判断是否完成）
        // 将工具输出转换为字符串，确保 JSON 结构正确显示
        let tool_result_str = serde_json::to_string_pretty(&observation.output)
            .unwrap_or_else(|_| serde_json::to_string(&observation.output).unwrap_or_default());

        let prompt_context = Self::with_intent_flags(
            coarse_step,
            PromptContext::new()
                .with_variable("coarse_step", serde_json::json!({
                    "intent": coarse_step.intent,
                    "expected_output": coarse_step.expected_output,
                }))
                .with_variable("tool_result", serde_json::json!(tool_result_str)),
        );

        let prompt = self.prompt_manager.render("planning/react_observe", &prompt_context).await
            .map_err(|e| anyhow::anyhow!("Failed to render template planning/react_observe: {}", e))?;

        debug!(
            target: "react_prompt",
            "[planning/react_observe] step={} chars={}\n{}",
            coarse_step.id,
            prompt.chars().count(),
            prompt
        );

        // 调用 LLM
        let response = self.call_llm(&prompt).await?;
        
        // 使用 prompt_manager 解析响应
        #[derive(serde::Deserialize)]
        struct ObserveResponse {
            is_complete: bool,
            reasoning: String,
        }
        
        let observe_response: ObserveResponse = self.prompt_manager.parse_response("planning/react_observe", &response).await?;
        
        Ok(ObserveResult {
            is_complete: observe_response.is_complete,
            summary: observe_response.reasoning,
        })
    }
    
    fn is_complete(&self, observation: &Observation) -> bool {
        // 如果有错误，未完成
        if observation.error.is_some() {
            return false;
        }
        
        // 如果输出为 null，未完成
        if observation.output.is_null() {
            return false;
        }
        
        // 默认认为已完成（可以根据具体场景调整）
        true
    }
    
    fn name(&self) -> &str {
        "DefaultReActAgent"
    }
}
