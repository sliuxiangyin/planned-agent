use std::sync::Arc;
use async_trait::async_trait;
use anyhow::{Result, anyhow};
use serde_json::json;

use planned_agent_core::planner::coarse::{
    CoarsePlanner, CoarseGrainedPlan, CoarsePlanValidationResult
};
use planned_agent_core::ai::AiClient;
use planned_agent_core::prompt::{PromptManager, PromptContext};
use planned_agent_core::types::{PlanContext, ChatCompletionRequest, Message, MessageRole, MessageContent};
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
    fn build_prompt_context(
        &self,
        input: &str,
        context: &PlanContext,
    ) -> Result<PromptContext> {
        let mut prompt_context = PromptContext::new();
        
        // 用户输入
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
}

#[async_trait]
impl<PM: PromptManager + Send + Sync> CoarsePlanner for LlmCoarsePlanner<PM> {
    /// 从用户输入生成粗粒度计划
    async fn generate_coarse_plan(
        &self,
        input: &str,
        context: &PlanContext,
    ) -> Result<CoarseGrainedPlan> {
        // 1. 构建提示上下文
        let prompt_context = self.build_prompt_context(input, context)?;

        // 2. 渲染提示模板（复用 PromptManager）
        let prompt = self.prompt_manager
            .render("planning/coarse_plan", &prompt_context)
            .await?;
        debug!("===========planning/coarse_plan: {} \r\n ================== \n\r ",prompt);
        // 3. 调用LLM生成计划（复用 AiClient）
        let request = ChatCompletionRequest {
            model: self.ai_client.model_name().to_string(),
            messages: vec![
                Message {
                    role: MessageRole::User,
                    content: Some(MessageContent::Text {
                        text: prompt,
                    }),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                }
            ],
            tools: None,
            temperature: Some(0.3),  // 允许更多创造性
            max_tokens: Some(2000),
            stream: false,
            extra: std::collections::HashMap::new(),
        };

        let response = self.ai_client.chat_completion(request).await?;
        
        // 提取响应内容
        let response_text = if let Some(choice) = response.choices.first() {
            if let Some(content) = &choice.message.content {
                match content {
                    MessageContent::Text { text } => text.clone(),
                    _ => return Err(anyhow!("不支持的响应内容类型")),
                }
            } else {
                return Err(anyhow!("无法从LLM响应中提取内容"));
            }
        } else {
            return Err(anyhow!("无法从LLM响应中提取内容"));
        };

        // 4. 解析响应为粗粒度计划（复用 PromptManager）
        let plan: CoarseGrainedPlan = self.prompt_manager
            .parse_response("planning/coarse_plan", &response_text)
            .await?;

        // 5. 验证步骤是否符合原子动作要求
        let validation_warnings = self.validate_atomic_steps(&plan);
        if !validation_warnings.is_empty() {
            debug!("步骤验证警告: {:?}", validation_warnings);
            // 可以选择返回警告或直接返回计划
            // 这里选择记录警告并返回计划
        }

        Ok(plan)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use planned_agent_core::planner::coarse::CoarseGrainedStep;

    // Mock AI客户端
    struct MockAiClient;

    #[async_trait]
    impl AiClient for MockAiClient {
        async fn chat_completion(&self, _request: ChatCompletionRequest) -> Result<planned_agent_core::types::ChatCompletionResponse> {
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

            Ok(planned_agent_core::types::ChatCompletionResponse {
                id: "test".to_string(),
                object: "chat.completion".to_string(),
                created: 0,
                model: "test".to_string(),
                choices: vec![planned_agent_core::types::Choice {
                    index: 0,
                    message: planned_agent_core::types::Message {
                        role: planned_agent_core::types::MessageRole::Assistant,
                        content: Some(planned_agent_core::types::MessageContent::Text {
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
            unimplemented!()
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
}
