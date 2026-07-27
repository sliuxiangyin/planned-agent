//! 结构化清洗 handler
//!
//! 对所有 Observation 的 output 进行选择性清洗，去除无意义的内容，保留原始数据结构。
//! 这是处理链的**第一环**，无条件执行，清洗结果传递给后续 handler。

use std::sync::Arc;
use tracing::{debug, info, warn};

use async_trait::async_trait;
use planned_agent_core::ai::AiClient;
use planned_agent_core::planner::react::Observation;
use planned_agent_core::types::{ChatCompletionRequest, Message, MessageContent, MessageRole};

use crate::planner::react::tool_result_router::ObservationPostHandler;

/// 结构化清洗 handler
///
/// 对任意工具输出的 output 字段进行选择性清洗：
/// 1. 保留：结构化数据（JSON、表格、列表）、关键信息、原始格式
/// 2. 去除：执行代码（JS/Python）、日志信息、调试输出、装饰性符号
/// 3. 通用：不依赖特定格式，不分析用户意图，只做清洗
///
/// 此 handler 无条件执行，作为处理链的第一环。
pub struct StructureCleanPostHandler {
    ai_client: Arc<dyn AiClient>,
}

impl StructureCleanPostHandler {
    pub fn new(ai_client: Arc<dyn AiClient>) -> Self {
        Self { ai_client }
    }
}

#[async_trait]
impl ObservationPostHandler for StructureCleanPostHandler {
    async fn handle(
        &self,
        obs: Observation,
        _current_intent: &str,
        _next_intent: &str,
    ) -> Observation {
        // 错误直接透传
        if obs.error.is_some() {
            return obs;
        }
        let raw_output = obs.output.to_string();
        debug!("原始输出：{:?}",raw_output);
        if raw_output.is_empty() {
            return obs;
        }

        // 检查是否需要清洗
        if !needs_cleaning(&raw_output) {
            debug!(
                "StructureClean: output ({}) doesn't need cleaning, passing through",
                raw_output.len()
            );
            return obs;
        }

        info!(
            "StructureClean: processing output ({} chars)",
            raw_output.len()
        );

        // 调用 LLM 选择性清洗
        match self.clean_output(&raw_output).await {
            Ok(cleaned) => {
                if cleaned != raw_output {
                    info!("StructureClean: cleaned output ({} -> {} chars)", raw_output.len(), cleaned.len());
                }
                let mut new_obs = obs;
                new_obs.output = serde_json::Value::String(cleaned);
                new_obs
            }
            Err(e) => {
                warn!("StructureClean: LLM call failed, passing through original output: {}", e);
                obs
            }
        }
    }
}

/// 快速判断输出是否需要清洗
fn needs_cleaning(output: &str) -> bool {
    let patterns = ["```", "Ran ", "await ", "console.", "### ", "\n---\n"];
    patterns.iter().any(|p| output.contains(p))
}

impl StructureCleanPostHandler {
    /// 选择性清洗输出
    async fn clean_output(&self, raw_output: &str) -> anyhow::Result<String> {
        let prompt = self.build_clean_prompt(raw_output);
        self.call_llm(&prompt).await
    }

    fn build_clean_prompt(&self, raw_output: &str) -> String {
        let rules = r#"你是一个信息清洗器。请对以下原始文本进行清洗。

## 清洗规则

必须保留：
- 结构化数据：JSON、数组、对象、表格、列表
- 关键信息：标题、链接、描述、数值、状态
- 原始数据结构：不改变数据的组织方式

必须去除：
- 执行代码：JavaScript、Python 等代码块（用 ``` 包裹的内容）
- 执行日志：如 "Ran Playwright code"、"console output" 等
- 装饰性标记：Markdown 标题分隔、分隔线
- 无意义的符号和格式化内容

重要：
- 只做清洗，不做分析或提取
- 不理解用户意图，只管去除噪音
- 不改变数据的原始结构，只去除周围的无意义内容
- 如果输出是纯 JSON/纯文本，直接返回（无需处理）
- 如果输出是 JSON + 代码，只提取 JSON 部分
- 保持输出可读性，不要过度压缩

## 原始文本"#;

        format!("{}\n\n{}\n\n## 要求\n直接返回清洗后的文本，不要返回 JSON 或任何解释：", rules, raw_output)
    }

    /// 调用 LLM
    async fn call_llm(&self, prompt: &str) -> anyhow::Result<String> {
        use std::collections::HashMap;

        let request = ChatCompletionRequest {
            model: self.ai_client.model_name().to_string(),
            messages: vec![Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text {
                    text: prompt.to_string(),
                }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            }],
            tools: None,
            temperature: Some(0.1),
            max_tokens: Some(8192),
            stream: false,
            extra: HashMap::new(),
        };

        let response = self.ai_client.chat_completion(request).await?;

        if let Some(choice) = response.choices.first() {
            if let Some(MessageContent::Text { text }) = &choice.message.content {
                return Ok(clean_markdown_wrapper(text.trim()));
            }
        }

        Err(anyhow::anyhow!("No text response from LLM"))
    }
}

/// 清理 markdown 代码块包装
fn clean_markdown_wrapper(text: &str) -> String {
    let text = text.trim();
    
    // 尝试提取代码块内容
    if let Some(after) = text.strip_prefix("```json\n") {
        if let Some(clean) = after.strip_suffix("```") {
            return clean.trim().to_string();
        }
    }
    if let Some(after) = text.strip_prefix("```\n") {
        if let Some(clean) = after.strip_suffix("```") {
            return clean.trim().to_string();
        }
    }
    
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_cleaning_detects_code_blocks() {
        assert!(needs_cleaning("```js\nawait page.click()\n```"));
        assert!(needs_cleaning("### Result\n[1,2,3]"));
        assert!(needs_cleaning("console.log('debug')"));
        assert!(needs_cleaning("Ran Playwright code"));
    }

    #[test]
    fn needs_cleaning_allows_clean_json() {
        assert!(!needs_cleaning(r#"[{"title":"test","link":"http://x.com"}]"#));
        assert!(!needs_cleaning(r#"{"key":"value"}"#));
        assert!(!needs_cleaning("plain text without code"));
    }

    #[test]
    fn clean_markdown_wrapper_handles_json_block() {
        let input = "```json\n{\"key\":\"value\"}\n```";
        let output = clean_markdown_wrapper(input);
        assert_eq!(output, r#"{"key":"value"}"#);
    }

    #[test]
    fn clean_markdown_wrapper_handles_plain_text() {
        let input = "plain text without wrapper";
        let output = clean_markdown_wrapper(input);
        assert_eq!(output, "plain text without wrapper");
    }
}
