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
use planned_agent_core::types::{
    PlanContext, ChatCompletionRequest, Message, MessageRole, MessageContent,
    ToolCall, FunctionCall, ToolType, ToolDefinition, FunctionDefinition,
};

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
    /// 对话消息列表（每次执行时初始化）
    messages: Vec<Message>,
    /// 待处理的 tool_call_id（用于构造 tool 消息）
    pending_tool_call_id: Option<String>,
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
        // 标准 handler：StructureClean + HtmlClean + BinaryTruncate
        let tool_result_router = ToolResultRouter::with_standard_handlers(
            html_sub_agent,
            50_000,
            ai_client.clone(),
        );

        Self {
            ai_client,
            prompt_manager,
            tool_registry,
            tool_result_router,
            config,
            messages: Vec::new(),
            pending_tool_call_id: None,
        }
    }
    
    /// 向 ReAct Prompt 注入完整步骤约束。
    fn with_step_context(
        prompt_context: PromptContext,
        coarse_step: &CoarseGrainedStep,
    ) -> PromptContext {
        let step_value = serde_json::to_value(coarse_step).unwrap_or_else(|_| {
            serde_json::json!({
                "intent": coarse_step.intent,
                "expected_output": coarse_step.expected_output,
            })
        });
        let data_requirements = serde_json::to_string_pretty(&coarse_step.data_requirements)
            .unwrap_or_else(|_| "[]".to_string());

        prompt_context
            .with_variable("coarse_step", step_value)
            .with_variable("data_requirements", serde_json::json!(data_requirements))
    }

    /// 构建后续步骤摘要字符串（用于 think prompt）
    fn build_remaining_steps_str(steps: Option<&[CoarseGrainedStep]>) -> String {
        match steps {
            Some(steps) if !steps.is_empty() => {
                steps.iter()
                    .map(|s| format!("- 步骤{}: {}", s.order, s.intent))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => "无".to_string(),
        }
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

    /// 初始化消息列表：System + User(goal)
    async fn init_messages(
        &mut self,
        _coarse_step: &CoarseGrainedStep,
        context: &PlanContext,
    ) -> Result<()> {
        // 获取用户目标
        let user_goal = context
            .metadata
            .get("user_goal")
            .and_then(|v| v.as_str())
            .unwrap_or("无")
            .to_string();

        // 渲染 System Prompt
        let system_prompt = self.prompt_manager.render("planning/react_system", &PromptContext::new()).await
            .map_err(|e| anyhow::anyhow!("Failed to render template planning/react_system: {}", e))?;

        self.messages = vec![
            Message {
                role: MessageRole::System,
                content: Some(MessageContent::Text { text: system_prompt }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            },
            Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text { text: format!("用户目标：\n{}", user_goal) }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            },
        ];
        
        Ok(())
    }

    /// 调用 LLM（使用 self.messages，支持传入 tools）
    ///
    /// 返回 OpenAI 的 assistant Message（含 content 和 tool_calls）。
    /// - `tools`：工具定义列表。think 阶段传 None，act 阶段按 step 类别过滤后传入。
    /// - think() 需要从 message.content 提取文本解析 Thought
    /// - act() 直接使用 message（含 tool_calls），符合 OpenAI 标准用法
    async fn call_llm_with_messages(
        &self,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Message> {
        let request = ChatCompletionRequest {
            model: self.ai_client.model_name().to_string(),
            messages: self.messages.clone(),
            tools,
            temperature: Some(0.3),
            max_tokens: Some(2000),
            stream: false,
            extra: Default::default(),
        };

        let response = self.ai_client.chat_completion(request).await?;

        if let Some(choice) = response.choices.into_iter().next() {
            Ok(choice.message)
        } else {
            Err(anyhow!("No choices in response"))
        }
    }

    /// 从 Message 中提取 content 文本（用于 think 解析 Thought）
    fn extract_text_content(message: &Message) -> Result<String> {
        match &message.content {
            Some(MessageContent::Text { text }) => Ok(text.clone()),
            _ => Err(anyhow!("No text content in message")),
        }
    }
    
    /// 调用 LLM（单 prompt，用于非对话场景如 observe）
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
    
    /// 根据 step 类别构建 ToolDefinition 列表
    ///
    /// 按 `categories` 过滤工具；若为空（None 或 []），兜底返回所有启用工具。
    /// 返回 None 表示无可用工具。
    fn build_tool_definitions(&self, categories: &[ToolCategory]) -> Option<Vec<ToolDefinition>> {
        let tools = if categories.is_empty() {
            self.tool_registry.get_all_tools()
        } else {
            self.tool_registry.get_tools_by_categories(categories)
        };

        if tools.is_empty() {
            return None;
        }

        Some(tools.iter().map(|t| ToolDefinition {
            r#type: ToolType::Function,
            function: FunctionDefinition {
                name: t.name.clone(),
                description: Some(t.description.clone()),
                parameters: Some(t.input_schema.clone()),
                strict: None,
            },
        }).collect())
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

    /// 生成 tool_call_id（基于时间戳）
    fn generate_tool_call_id(&self) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("call_{}", timestamp)
    }
    
    /// 推送 User 消息到 messages
    fn push_user_message(&mut self, text: String) {
        self.messages.push(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text }),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        });
    }
    
    /// 推送普通 Assistant 消息到 messages
    fn push_assistant_message(&mut self, text: String) {
        self.messages.push(Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text { text }),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        });
    }
    
    /// 推送带 tool_calls 的 Assistant 消息到 messages
    fn push_assistant_with_tool_calls(&mut self, text: String, tool_calls: Vec<ToolCall>) {
        self.messages.push(Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text { text }),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        });
    }

    /// 渲染 prompt 并推送为 User 消息
    async fn render_push_user_message(
        &mut self,
        template: &str,
        prompt_context: &PromptContext,
        coarse_step: &CoarseGrainedStep,
    ) -> Result<()> {
        let prompt = self.prompt_manager.render(template, prompt_context).await
            .map_err(|e| anyhow::anyhow!("Failed to render template {}: {}", template, e))?;

        debug!(
            target: "react_prompt",
            "[{}] step={} chars={}\n{}",
            template,
            coarse_step.id,
            prompt.chars().count(),
            prompt
        );

        self.push_user_message(prompt);
        Ok(())
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
        &mut self,
        coarse_step: &CoarseGrainedStep,
        context: &PlanContext,
    ) -> Result<ReActExecutionResult> {
        let start_time = Instant::now();
        let mut history = Vec::new();
        
        info!("Starting ReAct execution for step: {}", coarse_step.intent);
        
        // 初始化消息列表：System + User(goal)
        self.init_messages(coarse_step, context).await?;
        
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

            // 1. THINK - 思考（消息追加由 think() 内部完成）
            let thought = self.think(
                coarse_step,
                remaining_steps.as_deref(),
            ).await?;
            debug!("Thought: {} (confidence: {})", thought.reasoning, thought.confidence);

            // 2. ACT - 行动（消息追加由 act() 内部完成，含带 tool_calls 的 assistant 消息）
            let action = self.act(
                coarse_step,
                &thought,
            ).await?;
            debug!("Action: {} with params {}", action.tool_name, action.parameters);

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
            
            // 将工具结果作为 Tool 消息追加（符合 OpenAI API 标准）
            let tool_result_str = serde_json::to_string_pretty(&observation.output)
                .unwrap_or_else(|_| serde_json::to_string(&observation.output).unwrap_or_default());
            let tool_call_id = self.pending_tool_call_id.take().unwrap_or_else(|| "tool_result".to_string());
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
        &mut self,
        coarse_step: &CoarseGrainedStep,
        remaining_steps: Option<&[CoarseGrainedStep]>,
    ) -> Result<Thought> {
        // 1. 构建 prompt context
        // think 阶段不感知具体工具：完整 ToolDefinition 仅在 act 阶段通过 API tools 参数注入，
        // 此处只提供当前步骤、后续步骤与数据需求，便于模型规划下一步行动。
        let remaining_steps_str = Self::build_remaining_steps_str(remaining_steps);
        let prompt_context = Self::with_intent_flags(
            coarse_step,
            Self::with_step_context(
                PromptContext::new()
                    .with_variable("remaining_steps", serde_json::json!(remaining_steps_str)),
                coarse_step,
            ),
        );
    
        // 2. 渲染 prompt 并追加 User 消息
        self.render_push_user_message("planning/react_think", &prompt_context, coarse_step).await?;

        // 3. 调用 LLM（think 不需要工具，传 None）
        let assistant_msg = self.call_llm_with_messages(None).await?;

        // 4. 追加 Assistant 消息
        self.messages.push(assistant_msg.clone());

        // 5. 解析响应（提取 content 文本，解析为 Thought）
        let response = Self::extract_text_content(&assistant_msg)?;
        let thought: Thought = self.prompt_manager.parse_response("planning/react_think", &response).await?;
        Ok(thought)
    }
    
    async fn act(
        &mut self,
        coarse_step: &CoarseGrainedStep,
        thought: &Thought,
    ) -> Result<Action> {
        // 1. 构建 prompt context（不再传 tools，由 API 参数传递）
        let prompt_context = Self::with_intent_flags(
            coarse_step,
            Self::with_step_context(
                PromptContext::new()
                    .with_variable("thought", serde_json::json!({
                        "reasoning": thought.reasoning,
                        "plan": thought.plan
                    })),
                coarse_step,
            ),
        );

        // 2. 渲染 prompt 并追加 User 消息
        self.render_push_user_message("planning/react_act", &prompt_context, coarse_step).await?;

        // 3. 按 step 类别构建 tools 定义
        let step_categories = coarse_step.recommended_tool_categories.as_deref().unwrap_or(&[]);
        let tools = self.build_tool_definitions(step_categories);

        // 4. 调用 LLM（act 传入 tools，让 OpenAI API 走标准 tool_calls 返回）
        let assistant_msg = self.call_llm_with_messages(tools).await?;

        // 5. 直接把 OpenAI 返回的 assistant 消息原样推入 messages（含 tool_calls）
        self.messages.push(assistant_msg.clone());

        // 6. 从 tool_calls 提取第一个工具调用作为 Action
        let tool_call = assistant_msg.tool_calls.as_ref()
            .and_then(|calls| calls.first())
            .ok_or_else(|| anyhow!("No tool_calls in assistant response"))?;

        let tool_name = tool_call.function.name.clone();
        let parameters: Value = serde_json::from_str(&tool_call.function.arguments)
            .map_err(|e| anyhow!("Failed to parse tool arguments: {}", e))?;

        // 7. 保存 tool_call_id 供后续 tool 消息使用
        self.pending_tool_call_id = Some(tool_call.id.clone());

        Ok(Action {
            tool_name,
            parameters,
            reasoning: None,
        })
    }
    
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
        Ok(self
            .tool_result_router
            .process(raw_obs, &outcome.categories, current_intent, next_intent)
            .await)
    }
    
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
        if observation.error.is_some() {
            return false;
        }
        if observation.output.is_null() {
            return false;
        }
        true
    }
    
    fn name(&self) -> &str {
        "DefaultReActAgent"
    }
}
