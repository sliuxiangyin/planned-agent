use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::time::Instant;
use tracing::{error, info, warn};

use planned_agent_core::ai::AiClient;
use planned_agent_core::planner::coarse::CoarseGrainedStep;
use planned_agent_core::planner::react::StepResultStore;
use planned_agent_core::planner::react::*;
use planned_agent_core::prompt::{PromptContext, PromptManager};
use planned_agent_core::tool_registry::ToolCategory;
use planned_agent_core::types::{
    ChatCompletionRequest, FunctionDefinition, Message, MessageContent, MessageRole, PlanContext,
    Tool, ToolDefinition, ToolType,
};
use planned_agent_tool_manager::ToolRegistry;

use super::intent_handler::IntentHandler;
use super::intent_router::IntentRouter;
use super::sub_agents::html_clean_subagent::HtmlCleanSubAgent;
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
    /// 缓存的 intent handler 结果（避免 observe 时重复路由）
    cached_intent_vars: Vec<(&'static str, Value)>,
    /// 步骤结果存储（由 PlanAndExecuteAgent 传入）
    store: Option<StepResultStore>,
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
        let tool_result_router =
            ToolResultRouter::with_standard_handlers(html_sub_agent, 50_000, ai_client.clone());

        Self {
            ai_client,
            prompt_manager,
            tool_registry,
            tool_result_router,
            config,
            messages: Vec::new(),
            cached_intent_vars: Vec::new(),
            store: None,
        }
    }

    /// 设置步骤结果存储（由 PlanAndExecuteAgent 调用）
    pub fn set_store(&mut self, store: StepResultStore) {
        self.store = Some(store);
    }

    /// 向 ReAct Prompt 注入完整步骤约束。
    fn with_step_context(
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
    fn build_remaining_steps_str(steps: Option<&[CoarseGrainedStep]>) -> String {
        match steps {
            Some(steps) if !steps.is_empty() => steps
                .iter()
                .map(|s| format!("- 步骤{}: {}", s.order, s.intent))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => "无".to_string(),
        }
    }

    /// 人类可读的数据大小格式化
    fn format_size(bytes: usize) -> String {
        if bytes >= 1024 * 1024 {
            format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
        } else if bytes >= 1024 {
            format!("{:.1}KB", bytes as f64 / 1024.0)
        } else {
            format!("{}B", bytes)
        }
    }

    /// 工具输出保存到 logs 的阈值：超过 10KB 视为"大文件"
    const TOOL_OUTPUT_SAVE_THRESHOLD: usize = 5 * 1024;

    /// 将大工具输出保存到 logs/tool_outputs/ 目录，供后续清洗测试使用。
    ///
    /// 文件名格式：`{tool_name}_{timestamp}_{short_hash}.json`
    fn save_tool_output_to_log(
        tool_name: &str,
        parameters: &Value,
        raw_output: &Value,
        call_id: &str,
    ) {
        let output_str = serde_json::to_string(raw_output).unwrap_or_default();
        let output_len = output_str.len();

        if output_len <= Self::TOOL_OUTPUT_SAVE_THRESHOLD {
            return;
        }

        // 确保目录存在
        let log_dir = "logs/tool_outputs";
        if let Err(e) = fs::create_dir_all(log_dir) {
            warn!("无法创建工具输出日志目录 {}: {}", log_dir, e);
            return;
        }

        // 构建文件名：工具名 + 时间戳 + call_id 前8位
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let short_id: String = call_id.chars().take(8).collect();
        let filename = format!("{}/{}_{}_{}.json", log_dir, tool_name, timestamp, short_id);

        // 构建保存内容：含元数据和原始输出
        let record = serde_json::json!({
            "tool_name": tool_name,
            "timestamp": timestamp.to_string(),
            "call_id": call_id,
            "output_size": output_len,
            "output_size_human": Self::format_size(output_len),
            "parameters": parameters,
            "raw_output": raw_output,
        });

        match serde_json::to_string_pretty(&record) {
            Ok(pretty_json) => {
                if let Err(e) = fs::write(&filename, &pretty_json) {
                    warn!("保存工具输出到 {} 失败: {}", filename, e);
                } else {
                    info!(
                        "[DUMP] 大工具输出已保存 → {} ({})",
                        filename,
                        Self::format_size(output_len)
                    );
                }
            }
            Err(e) => {
                warn!("序列化工具输出失败: {}", e);
            }
        }
    }

    /// 根据 CoarseGrainedStep 解析主导意图，合并至 PromptContext。
    ///
    /// 首次调用时路由并缓存结果（init_messages），后续调用直接复用（observe）。
    fn with_intent_flags(
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

    /// 初始化消息列表：System(含 step 上下文) + User(goal)
    ///
    /// System prompt 动态渲染，注入当前步骤、后续步骤、可用工具名称、意图提示。
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
        let tool_names = self.get_tools_description(step_categories);

        // 构建后续步骤摘要
        let remaining_steps_str = Self::build_remaining_steps_str(remaining_steps);

        // 读取前序步骤结果（从共享 store），生成摘要避免系统提示膨胀
        let previous_results_str = match &self.store {
            Some(store) => {
                let guard = store
                    .read()
                    .map_err(|e| anyhow::anyhow!("StepResultStore 读锁获取失败: {}", e))?;
                if guard.is_empty() {
                    "（暂无前序步骤结果）".to_string()
                } else {
                    let mut lines: Vec<String> = Vec::new();
                    for (k, v) in guard.iter() {
                        if v.get("error").and_then(|e| e.as_bool()).unwrap_or(false) {
                            lines.push(format!("- {}: ❌ 失败 — 跳过此引用", k));
                        } else {
                            let size = serde_json::to_string(v)
                                .map(|s| s.len())
                                .unwrap_or(0);
                            lines.push(format!("- {}: ✓ ({})", k, Self::format_size(size)));
                        }
                    }
                    if lines.is_empty() {
                        "（暂无可用前序结果）".to_string()
                    } else {
                        lines.join("\n")
                    }
                }
            }
            None => "（无步骤结果存储）".to_string(),
        };

        // 动态渲染 System prompt（注入 step 上下文 + 工具名称 + intent_hints）
        let prompt_context = self.with_intent_flags(
            coarse_step,
            Self::with_step_context(
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
                    text: format!("用户目标：\n{}", user_goal),
                }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            },
        ];

        Ok(())
    }

    /// 调用 LLM，返回 OpenAI 的 assistant Message（含 content 和 tool_calls）。
    async fn call_llm_with_messages(
        &self,
        messages: &[Message],
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Message> {
        let request = ChatCompletionRequest {
            model: self.ai_client.model_name().to_string(),
            messages: messages.to_vec(),
            tools,
            temperature: Some(0.3),
            max_tokens: Some(8000),
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

    /// 调用 LLM（单 prompt，构建临时消息列表）
    async fn call_llm(&self, prompt: &str) -> Result<String> {
        let messages = vec![Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text {
                text: prompt.to_string(),
            }),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        }];
        let response = self.call_llm_with_messages(&messages, None).await?;
        Self::extract_text_content(&response)
    }

    /// 根据 categories 解析工具列表（空或 None 时兜底返回全部）
    fn resolve_tools(&self, categories: &[ToolCategory]) -> Vec<Tool> {
        if categories.is_empty() {
            self.tool_registry.get_all_tools()
        } else {
            self.tool_registry.get_tools_by_categories(categories)
        }
    }

    /// 根据 step 构建 ToolDefinition 列表（含类别工具 + 引用工具）
    ///
    /// 返回 None 表示无可用工具。
    fn build_tool_definitions(&self, step: &CoarseGrainedStep) -> Option<Vec<ToolDefinition>> {
        let categories = step.recommended_tool_categories.as_deref().unwrap_or(&[]);
        let mut tools = self.resolve_tools(categories);

        if !step.dependencies.is_empty() {
            // 只在分类解析结果中不存在时才添加 fetch_step_result，避免重复
            let already_has = tools.iter().any(|t| t.name == "builtin_fetch_step_result");
            if !already_has {
                if let Some(fetch_tool) = self.tool_registry.get_tool("builtin_fetch_step_result") {
                    tools.push(fetch_tool);
                }
            }
        }

        if tools.is_empty() {
            return None;
        }
        Some(
            tools
                .iter()
                .map(|t| ToolDefinition {
                    r#type: ToolType::Function,
                    function: FunctionDefinition {
                        name: t.name.clone(),
                        description: Some(t.description.clone()),
                        parameters: Some(t.input_schema.clone()),
                        strict: None,
                    },
                })
                .collect(),
        )
    }

    /// 获取可用工具列表描述（文本形式，注入 System prompt）
    fn get_tools_description(&self, categories: &[ToolCategory]) -> String {
        let tools = self.resolve_tools(categories);
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

    /// 推送带 tool_calls 的 Assistant 消息到 messages（供主循环直接 push LLM 返回的 Message 使用）
    fn push_assistant_message_raw(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    /// 执行AI处理工具
    pub async fn execute_ai_process(
        &self,
        parameters: &Value,
        start_time: Instant,
    ) -> Result<Observation> {
        let data = parameters.get("data").cloned().unwrap_or(Value::Null);
        let instruction = parameters
            .get("instruction")
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
            error: Some(format!(
                "AI处理失败（重试{}次后）: {}",
                max_retries,
                last_error
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "未知错误".to_string())
            )),
            duration_ms,
        })
    }

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
        let tools = self.build_tool_definitions(coarse_step);
        // 调用 LLM（带 tools），LLM 自主决策：输出 tool_calls 或回复 DONE
        let assistant_msg = self.call_llm_with_messages(&self.messages, tools).await?;

        // LLM 返回内容（原格式）
        info!(
            "[LLM] content={:?} tool_calls={:?}",
            assistant_msg.content.as_ref().and_then(|c| match c {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            }).unwrap_or("(no text)"),
            assistant_msg.tool_calls.as_ref().map(|v| v.iter().map(|tc| &tc.function.name).collect::<Vec<_>>())
        );

        // 检查是否声明 DONE（无 tool_calls 且 content 以 DONE 开头）
        if assistant_msg.tool_calls.is_none()
            || assistant_msg
                .tool_calls
                .as_ref()
                .map(|v| v.is_empty())
                .unwrap_or(true)
        {
            if let Ok(text) = Self::extract_text_content(&assistant_msg) {
                if text.trim().to_lowercase().starts_with("done") {
                    return Ok((vec![], assistant_msg));
                }
            }
        }

        // 从 tool_calls 提取全部 Action，每个带上 tool_call_id。
        // 若无 tool_calls 且不是 DONE → 视为 LLM 直接给出最终答案，按完成处理。
        let tool_calls = match assistant_msg.tool_calls.as_ref() {
            Some(tc) if !tc.is_empty() => tc,
            _ => {
                warn!(
                    "LLM returned no tool_calls and no DONE marker. Treating as completion. Content: {:?}",
                    assistant_msg
                        .content
                        .as_ref()
                        .and_then(|c| match c {
                            MessageContent::Text { text } => {
                                Some(&text[..text.len().min(200)])
                            }
                            _ => None,
                        })
                        .unwrap_or("(non-text)")
                );
                return Ok((vec![], assistant_msg));
            }
        };

        if tool_calls.len() > 1 {
            info!("LLM returned {} tool_calls, processing all sequentially.", tool_calls.len());
        }

        let actions: Vec<Action> = tool_calls
            .iter()
            .map(|tc| {
                let parameters: Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(Value::Null);
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

        // 初始化消息列表：System(含 step 上下文) + User(goal)
        self.init_messages(coarse_step, context, remaining_steps.as_deref())
            .await?;

        // 循环检测：记录最近的操作
        let mut last_action: Option<String> = None;
        let mut repeat_count: u32 = 0;
        let max_repeats = 3;

        let result = {
            let mut iteration = 0;
            'outer: loop {
                if iteration >= self.config.max_iterations {
                    break Ok(ReActExecutionResult::failure(
                        coarse_step.id.clone(),
                        format!("Exceeded max iterations: {}", self.config.max_iterations),
                        history,
                        self.config.max_iterations,
                        start_time.elapsed().as_millis() as u64,
                    ));
                }

                // 超时检查
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

                // 计算下一步意图（用于后处理路由）
                let next_intent = remaining_steps
                    .as_ref()
                    .and_then(|steps| steps.first().map(|step| step.intent.clone()))
                    .unwrap_or_default();
                let current_intent = coarse_step.intent.clone();

                // 1. THINK+ACT 合并 - LLM 自主输出 tool_calls 或 DONE
                let (actions, assistant_msg) = self.think_and_act(coarse_step).await?;

                // 2. 检查是否声明 DONE
                if actions.is_empty() {
                    let done_text = Self::extract_text_content(&assistant_msg).unwrap_or_default();
                    self.push_assistant_message_raw(assistant_msg);
                    break Ok(ReActExecutionResult::success(
                        coarse_step.id.clone(),
                        serde_json::json!({"status": "done", "content": done_text}),
                        history,
                        iteration + 1,
                        start_time.elapsed().as_millis() as u64,
                    ));
                }

                // 推入 Assistant 消息（含全部 tool_calls）
                self.push_assistant_message_raw(assistant_msg);

                // 3. 顺序执行所有 action
                let mut last_observation: Option<Observation> = None;
                for action in &actions {
                    // 循环检测：连续相同 action → 终止
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

                    // EXECUTE - 调用工具并经过后处理
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

                    // 将工具结果作为 Tool 消息追加
                    let tool_result_str = serde_json::to_string_pretty(&observation.output)
                        .unwrap_or_else(|_| {
                            serde_json::to_string(&observation.output).unwrap_or_default()
                        });
                    let tool_call_id = action
                        .tool_call_id
                        .clone()
                        .unwrap_or_else(|| "tool_result".to_string());
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

                // 4. OBSERVE - 所有 action 执行完后，用最后一条 observation 判断
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
                        ObserveResult {
                            is_complete: false,
                            summary: format!("Observe failed: {}", e),
                        }
                    }
                };

                // 更新最后一条历史记录的 thought（observe 结论）
                if let Some(last) = history.last_mut() {
                    last.thought.reasoning = observe_result.summary.clone();
                    last.thought.confidence = if observe_result.is_complete { 1.0 } else { 0.5 };
                }

                // 5. 判断是否完成
                if observe_result.is_complete {
                    break Ok(ReActExecutionResult::success(
                        coarse_step.id.clone(),
                        final_obs.output,
                        history,
                        iteration + 1,
                        start_time.elapsed().as_millis() as u64,
                    ));
                }
                iteration += 1;
            }
        };
        result
    }
    async fn execute_tool(
        &self,
        action: &Action,
        current_intent: &str,
        next_intent: &str,
    ) -> Result<Observation> {
        let start_time = Instant::now();
        let tool_name = action.tool_name.clone();
        let mut parameters = action.parameters.clone();

        // 工具调用日志
        info!(
            "[TOOL] name={} params={}",
            tool_name,
            serde_json::to_string(&parameters).unwrap_or_default()
        );

        // 特殊处理 builtin_fetch_step_result：直接从 store 查找并返回
        // ⚠️ 必须在 expand_refs 之前处理，因为需要原始引用字符串 "#E1"
        if tool_name == "builtin_fetch_step_result" {
            if let Some(ref store) = self.store {
                let guard = store
                    .read()
                    .map_err(|e| anyhow::anyhow!("StepResultStore 读锁失败: {}", e))?;

                let reference = parameters["reference"].as_str().unwrap_or("");

                let output = match guard.get(reference) {
                    Some(value) => {
                        let size = serde_json::to_string(value)
                            .map(|s| s.len())
                            .unwrap_or(0);
                        // 小数据直接返回内容，避免 LLM 困惑"只有元数据"
                        if size <= 800 {
                            value.clone()
                        } else {
                            serde_json::json!({
                                "reference": reference,
                                "size": size,
                                "hint": format!("数据较大（{}），请勿再次调用 fetch_step_result 获取同一引用。后续工具调用时直接把 \"{}\" 作为参数值传入即可，系统会自动展开。", Self::format_size(size), reference)
                            })
                        }
                    }
                    None => serde_json::json!({
                        "error": format!("未找到引用标识为 {} 的步骤结果。可用引用: {:?}",
                            reference, guard.keys().collect::<Vec<_>>())
                    }),
                };

                return Ok(Observation {
                    output,
                    is_complete: false,
                    error: None,
                    duration_ms: start_time.elapsed().as_millis() as u64,
                });
            }
        }

        // 自动展开参数中的 "#E1" 引用：扫描所有参数值，匹配则从 store 替换
        if let Some(ref store) = self.store {
            let guard = store
                .read()
                .map_err(|e| anyhow::anyhow!("StepResultStore 读锁失败: {}", e))?;
            expand_refs(&mut parameters, &guard);
        }

        // 特殊处理 ai_process：展开引用后走独立 AI 子流程
        if tool_name == "ai_process" {
            return self.execute_ai_process(&parameters, start_time).await;
        }

        let outcome_result = self.tool_registry.call_tool(&tool_name, parameters.clone()).await;

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

        // 大文件保存：原始工具输出超过阈值时存入 logs/tool_outputs/，供后续清洗测试
        let params_for_log = parameters.clone();
        let call_id = action
            .tool_call_id
            .as_deref()
            .unwrap_or("unknown");
        Self::save_tool_output_to_log(&tool_name, &params_for_log, &raw_obs.output, call_id);

        // 在 execute_tool 内部一站式完成"路由 + handler 链式执行"。
        Ok(self
            .tool_result_router
            .process(raw_obs, &outcome.categories, current_intent, next_intent)
            .await)
    }

    async fn observe(
        &mut self,
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

        // 渲染 observe prompt
        let tool_result_str = serde_json::to_string_pretty(&observation.output)
            .unwrap_or_else(|_| serde_json::to_string(&observation.output).unwrap_or_default());

        let prompt_context = self.with_intent_flags(
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

        // 用独立调用判断完成度（避免 messages 历史中的 tool_calls 模式污染 LLM 输出）
        let response = self.call_llm(&prompt).await?;

        // 解析响应
        #[derive(serde::Deserialize)]
        struct ObserveResponse {
            is_complete: bool,
            reasoning: String,
        }

        let observe_response: ObserveResponse = self
            .prompt_manager
            .parse_response("planning/react_observe", &response)
            .await?;

        // observe 结论回流到 messages（下轮 think+act 可见上下文）
        self.push_user_message(prompt);
        self.messages.push(Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text { text: response }),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        });

        Ok(ObserveResult {
            is_complete: observe_response.is_complete,
            summary: observe_response.reasoning,
        })
    }

    fn name(&self) -> &str {
        "DefaultReActAgent"
    }
}

/// 递归扫描 JSON 值中的 "#E" 引用字符串，自动从 store 展开为真实数据。
///
/// LLM 拿到 fetch_step_result 返回的引用后，自然会把 "#E1" 当作参数值传给其他工具。
/// 此函数在系统层透明替换，LLM 无需感知。
fn expand_refs(value: &mut Value, store: &HashMap<String, Value>) {
    match value {
        Value::String(s) => {
            // 严格匹配：必须为 "#E" + 纯数字（如 #E1、#E12）
            // 避免误匹配 hex 颜色(#FFF)、CSS 选择器(#header) 等
            let is_ref = s.len() >= 3
                && s.starts_with("#E")
                && s[2..].chars().all(|c| c.is_ascii_digit());
            if is_ref {
                if let Some(data) = store.get(s.as_str()) {
                    *value = data.clone();
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                expand_refs(v, store);
            }
        }
        Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                expand_refs(v, store);
            }
        }
        _ => {}
    }
}
