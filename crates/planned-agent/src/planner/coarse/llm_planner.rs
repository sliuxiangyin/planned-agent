use std::sync::Arc;
use async_trait::async_trait;
use anyhow::{Result, anyhow};
use futures::StreamExt;
use serde_json::json;

use planned_agent_core::planner::coarse::{
    CoarsePlanner, CoarseGrainedPlan, CoarsePlanValidationResult
};
use planned_agent_core::ai::AiClient;
use planned_agent_core::prompt::{PromptManager, PromptContext};
use planned_agent_core::ai::types::{ChatCompletionRequest, Message, MessageRole, MessageContent};
use planned_agent_core::planner::types::PlanContext;
use planned_agent_core::tool_registry::types::ToolCategory;
use tracing::debug;

/// 基于LLM的粗粒度计划器实现
///
/// 复用 AiClient 和 PromptManager 来生成粗粒度计划
pub struct LlmCoarsePlanner<PM: PromptManager> {
    /// AI客户端（复用已实现模块）
    ai_client: Arc<dyn AiClient>,
    /// 提示管理器（复用已实现模块）
    prompt_manager: Arc<PM>,
}

impl<PM: PromptManager> LlmCoarsePlanner<PM> {
    /// 创建新的粗粒度计划器
    ///
    /// # 参数
    /// - `ai_client`: AI客户端实例
    /// - `prompt_manager`: 提示管理器实例
    pub fn new(
        ai_client: Arc<dyn AiClient>,
        prompt_manager: Arc<PM>,
    ) -> Self {
        Self {
            ai_client,
            prompt_manager,
        }
    }

    /// 验证步骤是否符合原子动作要求
    fn validate_atomic_steps(&self, plan: &CoarseGrainedPlan) -> Vec<String> {
        let mut warnings = Vec::new();
        
        for step in &plan.steps {
            let intent = &step.intent;
            
            // 检查是否包含连接词
            let connecting_words = ["并", "和", "然后", "接着", "同时", "以及", "且"];
            for word in connecting_words {
                if intent.contains(word) {
                    warnings.push(format!(
                        "步骤 '{}' 包含连接词 '{}'，建议拆分为多个步骤",
                        step.id, word
                    ));
                }
            }
            
            // 检查是否包含多个动词（简单检查）
            let verb_count = self.count_verbs(intent);
            if verb_count > 1 {
                warnings.push(format!(
                    "步骤 '{}' 包含 {} 个动词，可能需要拆分",
                    step.id, verb_count
                ));
            }
        }
        
        warnings
    }
    
    /// 简单统计动词数量
    fn count_verbs(&self, text: &str) -> usize {
        // 简单的动词检测：查找常见的动词后缀或关键词
        let verb_indicators = [
            "获取", "读取", "写入", "删除", "创建", "修改", "查询", "调用",
            "发送", "接收", "处理", "分析", "计算", "转换", "提取", "过滤",
            "排序", "合并", "拆分", "验证", "检查", "测试", "执行", "运行",
            "启动", "停止", "暂停", "恢复", "连接", "断开", "打开", "关闭",
        ];
        
        let mut count = 0;
        for verb in verb_indicators {
            if text.contains(verb) {
                count += 1;
            }
        }
        
        // 如果没有找到明确的动词，返回1（默认至少有一个动词）
        if count == 0 { 1 } else { count }
    }

    /// 构建提示上下文
    ///
    /// 关键契约：`user_input` 必须原样保存用户原始输入字符串，不做任何归一化、
    /// 截断、实体提取或泛化处理。该变量由粗粒度计划 Prompt 用于生成步骤，
    /// 必须保证下游 CoarseGrainedStep 能追溯到原始关键词、人名、地名、URL、路径与数值。
    fn build_prompt_context(
        &self,
        input: &str,
        context: &PlanContext,
    ) -> Result<PromptContext> {
        let mut prompt_context = PromptContext::new();

        // 用户输入：必须原样透传，不得修改。
        prompt_context = prompt_context.with_variable("user_input", json!(input));
        
        // 计划上下文信息（将历史记录转换为上下文字符串）
        let context_str = if context.history.is_empty() {
            "无历史上下文".to_string()
        } else {
            format!("历史对话记录：\n{}", context.history.join("\n"))
        };
        prompt_context = prompt_context.with_variable("context", json!(context_str));
        
        // 可用工具分类列表
        let categories = ToolCategory::all();
        let categories_str = categories.iter()
            .map(|c| format!("- {:?}（{}）", c, c.description()))
            .collect::<Vec<_>>()
            .join("\n");
        prompt_context = prompt_context.with_variable("available_categories", json!(categories_str));
        
        Ok(prompt_context)
    }

    /// 流式生成粗粒度计划
    ///
    /// 与 [`CoarsePlanner::generate_coarse_plan`] 返回相同的 `CoarseGrainedPlan`，
    /// 区别在于 LLM 调用走 `chat_completion_stream`，每个增量文本片段通过
    /// `on_chunk` 回调（同步 `FnMut`）实时下发给调用方，便于 GUI 边收边显。
    ///
    /// 内部仍按现有 `parse_response` 流程解析完整文本，因此 `CoarseGrainedPlan`
    /// 的结构、字段、验证逻辑与同步路径完全一致。
    ///
    /// # 参数
    /// - `input`: 用户原始输入（与同步版本语义一致，原样透传到 Prompt）
    /// - `context`: 计划上下文
    /// - `on_chunk`: 每收到一段增量文本片段即调用一次，片段可能为单 token 或多 token 块
    pub async fn generate_coarse_plan_stream<F>(
        &self,
        input: &str,
        context: &PlanContext,
        mut on_chunk: F,
    ) -> Result<CoarseGrainedPlan>
    where
        F: FnMut(&str) + Send,
    {
        // 1. 构建提示上下文（复用同步路径逻辑）
        let prompt_context = self.build_prompt_context(input, context)?;

        // 2. 渲染提示模板（复用 PromptManager）
        let prompt = self.prompt_manager
            .render("planning/coarse_plan", &prompt_context)
            .await?;
        debug!(
            "===========planning/coarse_plan: {} \r\n ================== \n\r ",
            prompt
        );

        // 3. 构造 stream=true 请求
        let request = ChatCompletionRequest {
            model: self.ai_client.model_name().to_string(),
            messages: vec![Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text { text: prompt }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            }],
            tools: None,
            temperature: Some(0.3),
            max_tokens: Some(8000),
            stream: true,
            extra: std::collections::HashMap::new(),
        };

        // 4. 调用流式接口
        let response_stream = self.ai_client.chat_completion_stream(request).await?;

        // 5. 拉取 chunk，回调 + 累积
        let mut full_text = String::new();
        let mut stream = response_stream.stream;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            if let Some(choice) = chunk.choices.first() {
                if let Some(content) = &choice.delta.content {
                    if !content.is_empty() {
                        on_chunk(content);
                        full_text.push_str(content);
                    }
                }
            }
        }

        if full_text.is_empty() {
            return Err(anyhow!("LLM 流式响应为空，未产出任何文本片段"));
        }

        // 6. 解析完整文本（复用 PromptManager 的 markdown 围栏剥离 / 引号修复）
        let plan: CoarseGrainedPlan = self.prompt_manager
            .parse_response("planning/coarse_plan", &full_text)
            .await?;

        // 7. 验证步骤是否符合原子动作要求
        let validation_warnings = self.validate_atomic_steps(&plan);
        if !validation_warnings.is_empty() {
            debug!("步骤验证警告: {:?}", validation_warnings);
        }

        Ok(plan)
    }

    /// 从执行轨迹总结提炼粗粒度计划（灵活模式）。
    ///
    /// 使用 `planning/flexible_to_coarse` 模板，输入为 AI 自然语言总结文本，
    /// 输出与 [`CoarsePlanner::generate_coarse_plan`] 相同的 `CoarseGrainedPlan`。
    ///
    /// # 参数
    /// - `trace_summary`: AI 执行轨迹的完整总结文本
    /// - `on_chunk`: 流式回调（与周密模式一致）
    pub async fn generate_from_trace_stream<F>(
        &self,
        trace_summary: &str,
        mut on_chunk: F,
    ) -> Result<CoarseGrainedPlan>
    where
        F: FnMut(&str) + Send,
    {
        // 1. 构建提示上下文（仅 trace_summary 变量）
        let prompt_context = PromptContext::new()
            .with_variable("trace_summary", json!(trace_summary));

        // 2. 渲染 flexible_to_coarse 模板
        let prompt = self.prompt_manager
            .render("planning/flexible_to_coarse", &prompt_context)
            .await?;
        debug!(
            "===========planning/flexible_to_coarse: {} \r\n ==================",
            prompt
        );

        // 3. 构造 stream=true 请求
        let request = ChatCompletionRequest {
            model: self.ai_client.model_name().to_string(),
            messages: vec![Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text { text: prompt }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            }],
            tools: None,
            temperature: Some(0.3),
            max_tokens: Some(8000),
            stream: true,
            extra: std::collections::HashMap::new(),
        };

        // 4. 调用流式接口
        let response_stream = self.ai_client.chat_completion_stream(request).await?;

        // 5. 拉取 chunk，回调 + 累积
        let mut full_text = String::new();
        let mut stream = response_stream.stream;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            if let Some(choice) = chunk.choices.first() {
                if let Some(content) = &choice.delta.content {
                    if !content.is_empty() {
                        on_chunk(content);
                        full_text.push_str(content);
                    }
                }
            }
        }

        if full_text.is_empty() {
            return Err(anyhow!("LLM 流式响应为空，未产出任何文本片段"));
        }

        // 6. 解析完整文本（使用 flexible_to_coarse 模板的 output_schema）
        let mut plan: CoarseGrainedPlan = self.prompt_manager
            .parse_response("planning/flexible_to_coarse", &full_text)
            .await?;

        // 6.5 从原始响应中提取 output_schema 并写入计划
        let schema = extract_output_schema(&full_text);
        if !schema.is_empty() {
            plan.output_schema = Some(schema);
        }

        // 7. 验证原子步骤
        let validation_warnings = self.validate_atomic_steps(&plan);
        if !validation_warnings.is_empty() {
            debug!("步骤验证警告: {:?}", validation_warnings);
        }

        Ok(plan)
    }

    /// `generate_from_trace_stream` 的便捷版，丢弃流式回调。
    ///
    /// `output_schema` 已由 [`generate_from_trace_stream`] 内联提取并写入
    /// `CoarseGrainedPlan::output_schema`，调用方直接从 plan 读取即可。
    pub async fn generate_from_trace(
        &self,
        trace_summary: &str,
    ) -> Result<CoarseGrainedPlan> {
        self.generate_from_trace_stream(trace_summary, |_chunk| {}).await
    }
}

#[async_trait]
impl<PM: PromptManager + Send + Sync> CoarsePlanner for LlmCoarsePlanner<PM> {
    /// 从用户输入生成粗粒度计划
    ///
    /// 委托给 [`LlmCoarsePlanner::generate_coarse_plan_stream`]，丢弃回调，
    /// 仅保留完整解析后的 [`CoarseGrainedPlan`]。返回值与原同步实现完全一致。
    async fn generate_coarse_plan(
        &self,
        input: &str,
        context: &PlanContext,
    ) -> Result<CoarseGrainedPlan> {
        let mut full_text = String::new();
        self.generate_coarse_plan_stream(input, context, |chunk| {
            full_text.push_str(chunk);
        })
        .await
    }

    /// 验证粗粒度计划的合法性
    async fn validate_coarse_plan(
        &self,
        plan: &CoarseGrainedPlan,
    ) -> Result<CoarsePlanValidationResult> {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        // 检查步骤数量
        if plan.steps.is_empty() {
            errors.push("计划没有步骤".to_string());
        }

        // 检查步骤ID唯一性
        let step_ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
        let unique_ids: std::collections::HashSet<&str> = step_ids.iter().cloned().collect();
        if step_ids.len() != unique_ids.len() {
            errors.push("步骤ID不唯一".to_string());
        }

        // 检查结果引用唯一性
        let refs: Vec<&str> = plan.steps.iter().map(|s| s.result_reference.as_str()).collect();
        let unique_refs: std::collections::HashSet<&str> = refs.iter().cloned().collect();
        if refs.len() != unique_refs.len() {
            errors.push("结果引用不唯一".to_string());
        }

        // 检查依赖是否存在
        for step in &plan.steps {
            for dep in &step.dependencies {
                if !refs.contains(&dep.as_str()) {
                    warnings.push(format!("步骤 {} 依赖的 {} 不存在", step.id, dep));
                }
            }
        }

        Ok(CoarsePlanValidationResult {
            valid: errors.is_empty(),
            errors,
            warnings,
        })
    }

    /// 获取计划生成器名称
    fn name(&self) -> &str {
        "LlmCoarsePlanner"
    }
}

/// 从 LLM 原始响应中提取 `## 输出格式` 段落。
///
/// 格式示例：
/// ```text
/// ## 输出格式
/// JSON 对象数组，每项含 name(string)、url(string)、summary(string)
/// ```
fn extract_output_schema(raw_text: &str) -> String {
    // 查找 "## 输出格式" 标记
    let marker = "## 输出格式";
    if let Some(pos) = raw_text.find(marker) {
        let after_marker = &raw_text[pos + marker.len()..];
        // 取到下一个 ## 标题或文档结尾
        let end = after_marker.find("\n## ").unwrap_or(after_marker.len());
        after_marker[..end].trim().to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::collections::HashMap;
    use planned_agent_core::planner::coarse::CoarseGrainedStep;

    // 用于捕获最近一次 LLM 请求的 Mock AI 客户端
    struct CapturingMockAiClient {
        last_request: Mutex<Option<ChatCompletionRequest>>,
    }

    impl CapturingMockAiClient {
        fn new() -> Self {
            Self {
                last_request: Mutex::new(None),
            }
        }

        fn captured_prompt(&self) -> Option<String> {
            let guard = self.last_request.lock().unwrap();
            guard.as_ref().and_then(|req| {
                req.messages.first().and_then(|msg| {
                    if let Some(MessageContent::Text { text }) = &msg.content {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
            })
        }
    }

    #[async_trait]
    impl AiClient for CapturingMockAiClient {
        async fn chat_completion(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<planned_agent_core::ai::types::ChatCompletionResponse> {
            // 捕获请求内容，用于后续断言
            *self.last_request.lock().unwrap() = Some(request);

            // 返回保留“安仁乡”的模拟计划（模拟理想 LLM 行为）
            let response_json = r##"{
                "id": "plan-anren",
                "title": "搜索安仁乡",
                "description": "打开百度搜索安仁乡并整理前三条结果",
                "steps": [
                    {
                        "id": "step-1",
                        "order": 1,
                        "intent": "打开百度首页",
                        "expected_output": "百度首页加载完成",
                        "result_reference": "#E1",
                        "dependencies": [],
                        "data_requirements": [],
                        "recommended_tool_categories": ["Browser"]
                    },
                    {
                        "id": "step-2",
                        "order": 2,
                        "intent": "在百度搜索框输入'安仁乡'并执行搜索",
                        "expected_output": "搜索结果页面，包含与'安仁乡'相关的结果",
                        "result_reference": "#E2",
                        "dependencies": ["#E1"],
                        "data_requirements": [
                            {
                                "name": "search_keyword",
                                "description": "搜索关键词",
                                "required": true,
                                "source_hint": "用户原始输入：搜索安仁乡"
                            }
                        ],
                        "recommended_tool_categories": ["Browser"]
                    },
                    {
                        "id": "step-3",
                        "order": 3,
                        "intent": "提取安仁乡搜索结果的前三条",
                        "expected_output": "前三条安仁乡相关搜索结果",
                        "result_reference": "#E3",
                        "dependencies": ["#E2"],
                        "data_requirements": [],
                        "recommended_tool_categories": ["Browser", "Data"]
                    }
                ],
                "complexity": "simple",
                "risk_level": "low"
            }"##;

            Ok(planned_agent_core::ai::types::ChatCompletionResponse {
                id: "test".to_string(),
                object: "chat.completion".to_string(),
                created: 0,
                model: "test".to_string(),
                choices: vec![planned_agent_core::ai::types::Choice {
                    index: 0,
                    message: planned_agent_core::ai::types::Message {
                        role: planned_agent_core::ai::types::MessageRole::Assistant,
                        content: Some(planned_agent_core::ai::types::MessageContent::Text {
                            text: response_json.to_string(),
                        }),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                        reasoning_content: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
                usage: None,
                system_fingerprint: None,
            })
        }

        async fn chat_completion_stream(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<planned_agent_core::ai::ChatCompletionStream> {
            // 复用 chat_completion 的硬编码响应，作为单 chunk 推流
            let _ = self.chat_completion(request).await?;
            let text = r##"{
                "id": "plan-anren",
                "title": "搜索安仁乡",
                "description": "打开百度搜索安仁乡并整理前三条结果",
                "steps": [
                    {
                        "id": "step-1",
                        "order": 1,
                        "intent": "打开百度首页",
                        "expected_output": "百度首页加载完成",
                        "result_reference": "#E1",
                        "dependencies": [],
                        "data_requirements": [],
                        "recommended_tool_categories": ["Browser"]
                    },
                    {
                        "id": "step-2",
                        "order": 2,
                        "intent": "在百度搜索框输入'安仁乡'并执行搜索",
                        "expected_output": "搜索结果页面，包含与'安仁乡'相关的结果",
                        "result_reference": "#E2",
                        "dependencies": ["#E1"],
                        "data_requirements": [
                            {
                                "name": "search_keyword",
                                "description": "搜索关键词",
                                "required": true,
                                "source_hint": "用户原始输入：搜索安仁乡"
                            }
                        ],
                        "recommended_tool_categories": ["Browser"]
                    },
                    {
                        "id": "step-3",
                        "order": 3,
                        "intent": "提取安仁乡搜索结果的前三条",
                        "expected_output": "前三条安仁乡相关搜索结果",
                        "result_reference": "#E3",
                        "dependencies": ["#E2"],
                        "data_requirements": [],
                        "recommended_tool_categories": ["Browser", "Data"]
                    }
                ],
                "complexity": "simple",
                "risk_level": "low"
            }"##;
            use futures::stream;
            let chunk = planned_agent_core::ai::types::ChatCompletionChunk {
                id: "capture-mock".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 0,
                model: "mock-model".to_string(),
                choices: vec![planned_agent_core::ai::types::ChunkChoice {
                    index: 0,
                    delta: planned_agent_core::ai::types::DeltaMessage {
                        role: None,
                        content: Some(text.to_string()),
                        tool_calls: None,
                        reasoning_content: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
                system_fingerprint: None,
                usage: None,
            };
            let s = stream::iter(std::iter::once(Ok::<_, anyhow::Error>(chunk)));
            Ok(planned_agent_core::ai::ChatCompletionStream::new(Box::new(s)))
        }

        fn provider_name(&self) -> &str {
            "mock"
        }

        fn model_name(&self) -> &str {
            "mock-model"
        }

        fn default_config(&self) -> ChatCompletionRequest {
            unimplemented!()
        }
    }

    // Mock AI客户端
    struct MockAiClient;

    #[async_trait]
    impl AiClient for MockAiClient {
        async fn chat_completion(&self, _request: ChatCompletionRequest) -> Result<planned_agent_core::ai::types::ChatCompletionResponse> {
            // 返回模拟响应
            let response_json = "{
                \"title\": \"测试计划\",
                \"description\": \"这是一个测试计划\",
                \"steps\": [
                    {
                        \"id\": \"step-1\",
                        \"intent\": \"获取用户信息\",
                        \"expected_output\": \"用户信息JSON\",
                        \"result_reference\": \"#E1\",
                        \"dependencies\": [],
                        \"data_requirements\": []
                    }
                ],
                \"complexity\": \"simple\",
                \"risk_level\": \"low\"
            }";

            Ok(planned_agent_core::ai::types::ChatCompletionResponse {
                id: "test".to_string(),
                object: "chat.completion".to_string(),
                created: 0,
                model: "test".to_string(),
                choices: vec![planned_agent_core::ai::types::Choice {
                    index: 0,
                    message: planned_agent_core::ai::types::Message {
                        role: planned_agent_core::ai::types::MessageRole::Assistant,
                        content: Some(planned_agent_core::ai::types::MessageContent::Text {
                            text: response_json.to_string(),
                        }),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                        reasoning_content: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
                usage: None,
                system_fingerprint: None,
            })
        }

        async fn chat_completion_stream(&self, _request: ChatCompletionRequest) -> Result<planned_agent_core::ai::ChatCompletionStream> {
            // 复用 chat_completion 的硬编码响应，作为单 chunk 推流
            let text = r##"{
                "title": "测试计划",
                "description": "这是一个测试计划",
                "steps": [
                    {
                        "id": "step-1",
                        "intent": "获取用户信息",
                        "expected_output": "用户信息JSON",
                        "result_reference": "#E1",
                        "dependencies": [],
                        "data_requirements": []
                    }
                ],
                "complexity": "simple",
                "risk_level": "low"
            }"##;
            use futures::stream;
            let chunk = planned_agent_core::ai::types::ChatCompletionChunk {
                id: "mock".to_string(),
                object: "chat.completion.chunk".to_string(),
                created: 0,
                model: "mock-model".to_string(),
                choices: vec![planned_agent_core::ai::types::ChunkChoice {
                    index: 0,
                    delta: planned_agent_core::ai::types::DeltaMessage {
                        role: None,
                        content: Some(text.to_string()),
                        tool_calls: None,
                        reasoning_content: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                }],
                system_fingerprint: None,
                usage: None,
            };
            let s = stream::iter(std::iter::once(Ok::<_, anyhow::Error>(chunk)));
            Ok(planned_agent_core::ai::ChatCompletionStream::new(Box::new(s)))
        }

        fn provider_name(&self) -> &str {
            "mock"
        }

        fn model_name(&self) -> &str {
            "mock-model"
        }

        fn default_config(&self) -> ChatCompletionRequest {
            unimplemented!()
        }
    }

    // Mock PromptManager
    struct MockPromptManager;

    #[async_trait]
    impl PromptManager for MockPromptManager {
        async fn load_template(&self, _name: &str) -> Result<planned_agent_core::prompt::PromptTemplate> {
            unimplemented!()
        }

        async fn render(&self, _name: &str, _context: &planned_agent_core::prompt::PromptContext) -> Result<String> {
            Ok("测试提示".to_string())
        }

        async fn list_prompts(&self) -> Result<Vec<planned_agent_core::prompt::PromptInfo>> {
            unimplemented!()
        }

        async fn exists(&self, _name: &str) -> Result<bool> {
            unimplemented!()
        }

        async fn reload(&self) -> Result<()> {
            unimplemented!()
        }

        async fn get_output_schema(&self, _name: &str) -> Result<Option<serde_json::Value>> {
            unimplemented!()
        }

        async fn parse_response<T: serde::de::DeserializeOwned>(&self, _name: &str, _response: &str) -> Result<T> {
            // 返回模拟的 CoarseGrainedPlan
            let plan_json = "{
                \"id\": \"test-plan\",
                \"title\": \"测试计划\",
                \"description\": \"这是一个测试计划\",
                \"steps\": [
                    {
                        \"id\": \"step-1\",
                        \"order\": 1,
                        \"intent\": \"获取用户信息\",
                        \"expected_output\": \"用户信息JSON\",
                        \"result_reference\": \"#E1\",
                        \"dependencies\": [],
                        \"data_requirements\": []
                    }
                ],
                \"complexity\": \"simple\",
                \"risk_level\": \"low\"
            }";
            
            let plan: T = serde_json::from_str(plan_json)
                .map_err(|e| anyhow!("解析失败: {}", e))?;
            Ok(plan)
        }

        async fn validate_response(&self, _name: &str, _response: &str) -> Result<bool> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn test_generate_coarse_plan() {
        let ai_client = Arc::new(MockAiClient);
        let prompt_manager = Arc::new(MockPromptManager);
        
        let planner = LlmCoarsePlanner::new(ai_client, prompt_manager);
        
        let context = PlanContext {
            user_id: Some("test-user".to_string()),
            session_id: Some("test-session".to_string()),
            history: vec![],
            metadata: std::collections::HashMap::new(),
        };
        
        let result = planner.generate_coarse_plan("测试任务", &context).await;
        assert!(result.is_ok());
        
        let plan = result.unwrap();
        assert_eq!(plan.title, "测试计划");
        assert_eq!(plan.steps.len(), 1);
    }

    // 真实 PromptManager，用于在测试中实际渲染 planning/coarse_plan 模板
    struct RealPromptManager {
        inner: planned_agent_prompt_manager::FilePromptManager,
    }

    #[async_trait]
    impl PromptManager for RealPromptManager {
        async fn load_template(
            &self,
            name: &str,
        ) -> Result<planned_agent_core::prompt::PromptTemplate> {
            self.inner.load_template(name).await
        }

        async fn render(
            &self,
            name: &str,
            context: &planned_agent_core::prompt::PromptContext,
        ) -> Result<String> {
            self.inner.render(name, context).await
        }

        async fn list_prompts(
            &self,
        ) -> Result<Vec<planned_agent_core::prompt::PromptInfo>> {
            self.inner.list_prompts().await
        }

        async fn exists(&self, name: &str) -> Result<bool> {
            self.inner.exists(name).await
        }

        async fn reload(&self) -> Result<()> {
            self.inner.reload().await
        }

        async fn get_output_schema(&self, name: &str) -> Result<Option<serde_json::Value>> {
            self.inner.get_output_schema(name).await
        }

        async fn parse_response<T: serde::de::DeserializeOwned>(
            &self,
            name: &str,
            response: &str,
        ) -> Result<T> {
            self.inner.parse_response(name, response).await
        }

        async fn validate_response(&self, name: &str, response: &str) -> Result<bool> {
            self.inner.validate_response(name, response).await
        }
    }

    async fn build_real_prompt_manager_async() -> Arc<RealPromptManager> {
        use std::path::PathBuf;
        use planned_agent_prompt_manager::{FilePromptManager, PromptManagerConfig};
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // planned-agent 在 crates/planned-agent；向上两级到仓库根
        let project_root = manifest_dir
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let config = PromptManagerConfig {
            prompt_dir: project_root.join("prompts"),
            ..Default::default()
        };
        let pm = FilePromptManager::new(config).unwrap();
        pm.initialize().await.unwrap();
        Arc::new(RealPromptManager { inner: pm })
    }

    /// 端到端测试：模拟 LLM 行为，验证
    /// 1) 用户输入中的“安仁乡”被原样透传到 LLM 请求
    /// 2) coarse_plan Prompt 包含新增的实体保留强约束
    /// 3) 解析得到的 CoarseGrainedPlan 在所有 step.intent 中保留了“安仁乡”
    #[tokio::test(flavor = "current_thread")]
    async fn test_search_anren_preserves_entity_in_prompt_and_plan() {
        let ai_client = Arc::new(CapturingMockAiClient::new());
        let prompt_manager = build_real_prompt_manager_async().await;

        let planner = LlmCoarsePlanner::new(ai_client.clone(), prompt_manager);

        let context = PlanContext {
            user_id: Some("test-user".to_string()),
            session_id: Some("test-session".to_string()),
            history: vec![],
            metadata: HashMap::new(),
        };

        let user_input = "打开百度，搜索安仁乡，给出前三条相关信息并整理给我";
        let plan = planner
            .generate_coarse_plan(user_input, &context)
            .await
            .expect("生成粗粒度计划失败");

        // 1) 实际发送给 LLM 的 prompt 必须原样包含“安仁乡”
        let rendered_prompt = ai_client
            .captured_prompt()
            .expect("未捕获到 LLM 请求内容");
        assert!(
            rendered_prompt.contains(user_input),
            "渲染后 Prompt 必须原样保留用户输入:\n{}",
            rendered_prompt
        );
        assert!(
            rendered_prompt.contains("安仁乡"),
            "渲染后 Prompt 必须包含关键地名安仁乡"
        );
        // 2) Prompt 必须包含新增的实体保留强约束
        assert!(
            rendered_prompt.contains("用户实体保留约束"),
            "渲染后 Prompt 必须包含用户实体保留约束段"
        );
        assert!(
            rendered_prompt.contains("禁止使用") && rendered_prompt.contains("占位符"),
            "Prompt 必须显式禁止占位符"
        );

        // 3) 解析得到的步骤必须保留“安仁乡”
        assert!(!plan.steps.is_empty(), "计划必须至少包含一个步骤");
        for step in &plan.steps {
            // 步骤 intent 中必须出现 “安仁乡”（至少在搜索步骤中必须）
            let combined = format!(
                "{} {}",
                step.intent,
                step.expected_output
            );
            if step.intent.contains("搜索") || step.intent.contains("提取") {
                assert!(
                    combined.contains("安仁乡"),
                    "步骤 {} 的描述必须保留“安仁乡”: intent='{}', expected_output='{}'",
                    step.id,
                    step.intent,
                    step.expected_output
                );
            }
        }
    }

    #[tokio::test]
    async fn test_validate_coarse_plan() {
        let ai_client = Arc::new(MockAiClient);
        let prompt_manager = Arc::new(MockPromptManager);

        let planner = LlmCoarsePlanner::new(ai_client, prompt_manager);

        // 创建测试计划
        let plan = CoarseGrainedPlan::new(
            "test-plan".to_string(),
            "测试计划".to_string(),
            "测试描述".to_string(),
            vec![
                CoarseGrainedStep::new(
                    "step-1".to_string(),
                    1,
                    "步骤1".to_string(),
                    "输出1".to_string(),
                    "#E1".to_string(),
                ),
            ],
            planned_agent_core::planner::coarse::PlanComplexity::Simple,
            planned_agent_core::planner::coarse::RiskLevel::Low,
        );

        let result = planner.validate_coarse_plan(&plan).await;
        assert!(result.is_ok());

        let validation = result.unwrap();
        assert!(validation.valid);
        assert!(validation.errors.is_empty());
    }

    // ─── 流式 Mock：按预设片段序列推流 ──────────────────────────

    /// 推流式 Mock AI 客户端：忽略 prompt 内容，按 chunks 顺序逐片段推流
    struct StreamingMockAiClient {
        chunks: Vec<String>,
    }

    #[async_trait]
    impl AiClient for StreamingMockAiClient {
        async fn chat_completion(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<planned_agent_core::ai::types::ChatCompletionResponse> {
            unimplemented!("streaming mock 仅实现 chat_completion_stream")
        }

        async fn chat_completion_stream(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<planned_agent_core::ai::ChatCompletionStream> {
            use futures::stream;
            let items: Vec<planned_agent_core::ai::types::ChatCompletionChunk> = self
                .chunks
                .iter()
                .map(|text| planned_agent_core::ai::types::ChatCompletionChunk {
                    id: "stream-mock".to_string(),
                    object: "chat.completion.chunk".to_string(),
                    created: 0,
                    model: "mock-stream".to_string(),
                    choices: vec![planned_agent_core::ai::types::ChunkChoice {
                        index: 0,
                        delta: planned_agent_core::ai::types::DeltaMessage {
                            role: None,
                            content: Some(text.clone()),
                            tool_calls: None,
                            reasoning_content: None,
                        },
                        finish_reason: None,
                        logprobs: None,
                    }],
                    system_fingerprint: None,
                    usage: None,
                })
                .collect();
            let s = stream::iter(items.into_iter().map(Ok::<_, anyhow::Error>));
            Ok(planned_agent_core::ai::ChatCompletionStream::new(Box::new(s)))
        }

        fn provider_name(&self) -> &str {
            "mock-stream"
        }

        fn model_name(&self) -> &str {
            "mock-stream-model"
        }

        fn default_config(&self) -> ChatCompletionRequest {
            unimplemented!()
        }
    }

    /// 流式端到端测试：
    /// 1) 推流 N 个片段 → 回调被调 N 次
    /// 2) 片段按序拼接 == 完整 JSON
    /// 3) parse_response 真实解析 → 最终 CoarseGrainedPlan 字段正确
    #[tokio::test(flavor = "current_thread")]
    async fn test_generate_coarse_plan_stream_emits_chunks_and_parses_plan() {
        // 完整 JSON（将被切成片段推流）
        let full_json = r##"{"id":"plan-stream","title":"流式测试计划","description":"验证流式回调与解析","steps":[{"id":"step-1","order":1,"intent":"获取用户信息","expected_output":"用户信息JSON","result_reference":"#E1","dependencies":[],"data_requirements":[]}],"complexity":"simple","risk_level":"low"}"##;

        // 拆分为片段（模拟 LLM token-level 流式输出）
        // 使用 r##"..."## 双 # 定界以避免 "#E1" 内的 # 误闭合
        let chunks: Vec<String> = vec![
            r##"{"id":"##.to_string(),
            r##""plan-stream","##.to_string(),
            r##""title":"流式测试计划","description":"##.to_string(),
            r##""验证流式回调与解析","steps":[{"id":"step-1","order":1,"##.to_string(),
            r##""intent":"获取用户信息","expected_output":"##.to_string(),
            r##""用户信息JSON","result_reference":"#E1","dependencies":[],"##.to_string(),
            r##""data_requirements":[]}],"complexity":"simple","##.to_string(),
            r##""risk_level":"low"}"##.to_string(),
        ];

        let ai_client = Arc::new(StreamingMockAiClient {
            chunks: chunks.clone(),
        });
        let prompt_manager = build_real_prompt_manager_async().await;

        let planner = LlmCoarsePlanner::new(ai_client, prompt_manager);

        let context = PlanContext {
            user_id: Some("test-user".to_string()),
            session_id: Some("test-session".to_string()),
            history: vec![],
            metadata: std::collections::HashMap::new(),
        };

        let collected: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let plan = planner
            .generate_coarse_plan_stream("测试任务", &context, |chunk| {
                collected.lock().unwrap().push(chunk.to_string());
            })
            .await
            .expect("流式计划生成失败");

        // 1. 回调次数 == 片段数
        let chunks_received = collected.lock().unwrap();
        assert_eq!(
            chunks_received.len(),
            chunks.len(),
            "回调次数与片段数不一致"
        );

        // 2. 累积文本 == 完整 JSON
        let reconstructed: String = chunks_received.join("");
        assert_eq!(
            reconstructed, full_json,
            "累积文本与原始 JSON 不一致: got={}",
            reconstructed
        );

        // 3. 最终 plan 解析正确
        assert_eq!(plan.id, "plan-stream");
        assert_eq!(plan.title, "流式测试计划");
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].id, "step-1");
        assert_eq!(plan.steps[0].intent, "获取用户信息");
        assert_eq!(plan.steps[0].result_reference, "#E1");
    }

    /// 空流响应必须报错，不得 panic / 返回空 plan
    #[tokio::test(flavor = "current_thread")]
    async fn test_generate_coarse_plan_stream_empty_response_errors() {
        let ai_client = Arc::new(StreamingMockAiClient { chunks: vec![] });
        let prompt_manager = build_real_prompt_manager_async().await;

        let planner = LlmCoarsePlanner::new(ai_client, prompt_manager);

        let context = PlanContext {
            user_id: None,
            session_id: None,
            history: vec![],
            metadata: std::collections::HashMap::new(),
        };

        let result = planner
            .generate_coarse_plan_stream("测试任务", &context, |_chunk| {})
            .await;

        assert!(
            result.is_err(),
            "空流响应必须返回 Err，但得到了 {:?}",
            result.as_ref().map(|p| &p.title)
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("为空") || err_msg.contains("empty"),
            "错误消息应说明流式响应为空，实际: {}",
            err_msg
        );
    }
}
