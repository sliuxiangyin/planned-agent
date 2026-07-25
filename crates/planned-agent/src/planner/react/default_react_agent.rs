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
use planned_agent_tool_manager::ToolRegistry;
use planned_agent_core::types::{PlanContext, ChatCompletionRequest, Message, MessageRole, MessageContent};

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
}

impl<PM: PromptManager> DefaultReActAgent<PM> {
    /// 创建新的 DefaultReActAgent
    pub fn new(
        ai_client: Arc<dyn AiClient>,
        prompt_manager: Arc<PM>,
        tool_registry: Arc<ToolRegistry>,
        config: ReActAgentConfig,
    ) -> Self {
        Self {
            ai_client,
            prompt_manager,
            tool_registry,
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
    fn get_tools_description(&self) -> String {
        let tools = self.tool_registry.get_all_tools();
        let mut desc = String::new();
        
        for tool in &tools {
            desc.push_str(&format!(
                "- {}: {}\n  参数Schema：{}\n",
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
impl<PM: PromptManager> ReActAgent for DefaultReActAgent<PM> {
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
                match self.act(&thought, context).await {
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
            
            // 3. EXECUTE - 执行工具
            let raw_observation = match self.execute_tool(&action).await {
                Ok(obs) => {
                    debug!("Raw observation: error={:?}", obs.error);
                    obs
                }
                Err(e) => {
                    error!("Execute tool failed: {}", e);
                    // 创建失败的观察
                    Observation {
                        output: Value::Null,
                        is_complete: false,
                        error: Some(e.to_string()),
                        duration_ms: 0,
                    }
                }
            };
            
            // 4. OBSERVE - 分析工具执行结果，提取关键信息
            let observe_result = match self.observe(coarse_step, &raw_observation).await {
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
            
            // 记录历史
            history.push(ReActStep {
                thought: thought.clone(),
                action: action.clone(),
                observation: raw_observation.clone(),
            });
            
            // 5. 判断是否完成
            if observe_result.is_complete {
                info!("ReAct execution completed in {} iterations", iteration + 1);
                return Ok(ReActExecutionResult::success(
                    coarse_step.id.clone(),
                    raw_observation.output,
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
        let prompt_context = PromptContext::new()
            .with_variable("coarse_step", serde_json::json!({"intent": coarse_step.intent}))
            .with_variable("tools", serde_json::json!(self.get_tools_description()))
            .with_variable("history", serde_json::json!(history_str))
            .with_variable("previous_outputs", serde_json::json!(previous_outputs))
            .with_variable("remaining_steps", serde_json::json!(remaining_steps_str));
        
        let prompt = self.prompt_manager.render("planning/react_think", &prompt_context).await
            .map_err(|e| anyhow::anyhow!("Failed to render template planning/react_think: {}", e))?;
        
        // 调用 LLM
        let response = self.call_llm(&prompt).await?;
        
        // 使用 prompt_manager 解析响应
        let thought: Thought = self.prompt_manager.parse_response("planning/react_think", &response).await?;
        
        Ok(thought)
    }
    
    async fn act(
        &self,
        thought: &Thought,
        context: &PlanContext,
    ) -> Result<Action> {
        // 获取前序步骤的原始输出
        let previous_outputs = context.metadata.get("previous_outputs")
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
            .unwrap_or_else(|| "无".to_string());
        
        // 使用 prompt_manager 渲染 prompt
        let prompt_context = PromptContext::new()
            .with_variable("thought", serde_json::json!({
                "reasoning": thought.reasoning,
                "plan": thought.plan
            }))
            .with_variable("tools", serde_json::json!(self.get_tools_description()))
            .with_variable("previous_outputs", serde_json::json!(previous_outputs));
        
        let prompt = self.prompt_manager.render("planning/react_act", &prompt_context).await?;
        
        // 调用 LLM
        let response = self.call_llm(&prompt).await?;
        
        // 使用 prompt_manager 解析响应
        let action: Action = self.prompt_manager.parse_response("planning/react_act", &response).await?;
        
        Ok(action)
    }
    
    async fn execute_tool(
        &self,
        action: &Action,
    ) -> Result<Observation> {
        let start_time = Instant::now();
        let tool_name = action.tool_name.clone();
        let parameters = action.parameters.clone();
        
        // 特殊处理 ai_process 工具
        if tool_name == "ai_process" {
            return self.execute_ai_process(&parameters, start_time).await;
        }
        
        let tool_registry = self.tool_registry.clone();
        
        // 使用 spawn_blocking 来避免 RwLockReadGuard 的 Send 问题
        let result = tokio::task::spawn_blocking(move || {
            // 这里需要使用 block_on 来调用异步函数
            let rt = tokio::runtime::Handle::current();
            rt.block_on(tool_registry.call_tool(&tool_name, parameters))
        }).await?;
        
        let duration_ms = start_time.elapsed().as_millis() as u64;
        
        match result {
            Ok(tool_result) => {
                if tool_result.is_error {
                    Ok(Observation {
                        output: tool_result.content,
                        is_complete: false,
                        error: Some("Tool returned error".to_string()),
                        duration_ms,
                    })
                } else {
                    Ok(Observation {
                        output: tool_result.content,
                        is_complete: false, // 需要后续判断
                        error: None,
                        duration_ms,
                    })
                }
            }
            Err(e) => {
                Ok(Observation {
                    output: Value::Null,
                    is_complete: false,
                    error: Some(e.to_string()),
                    duration_ms,
                })
            }
        }
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
        
        let context = PromptContext::new()
            .with_variable("coarse_step", serde_json::json!({"intent": coarse_step.intent}))
            .with_variable("tool_result", serde_json::json!(tool_result_str));
        
        let prompt = self.prompt_manager.render("planning/react_observe", &context).await
            .map_err(|e| anyhow::anyhow!("Failed to render template planning/react_observe: {}", e))?;
        
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
