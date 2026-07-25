use std::sync::Arc;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use planned_agent_core::ai::AiClient;
use planned_agent_core::prompt::{PromptContext, PromptManager};
use planned_agent_core::types::{ChatCompletionRequest, Message, MessageRole, MessageContent};
use planned_agent_tool_manager::{ToolOutcome, ToolRegistry};

/// 格式决策接口，抽象后便于测试注入确定性的桩实现。
#[async_trait]
pub trait FormatDecider: Send + Sync {
    /// 决定 HTML 清洗的输出格式。
    /// 实现不应读取调用方的主上下文。
    async fn decide(
        &self,
        head_sample: &str,
        current_intent: &str,
        next_intent: &str,
    ) -> Result<String>;
}

/// 基于 `AiClient` 和 `PromptManager` 的默认 `FormatDecider` 实现。
/// 通过一次小型、隔离的 LLM 调用，确保主 ReAct 上下文不会暴露。
pub struct LlmFormatDecider<PM: PromptManager> {
    ai: Arc<dyn AiClient>,
    prompt_manager: Arc<PM>,
}

impl<PM: PromptManager> LlmFormatDecider<PM> {
    pub fn new(ai: Arc<dyn AiClient>, prompt_manager: Arc<PM>) -> Self {
        Self { ai, prompt_manager }
    }
}

#[async_trait]
impl<PM: PromptManager> FormatDecider for LlmFormatDecider<PM> {
    async fn decide(
        &self,
        head_sample: &str,
        current_intent: &str,
        next_intent: &str,
    ) -> Result<String> {
        #[derive(Deserialize)]
        struct FormatChoice {
            format: String,
        }

        let prompt_context = PromptContext::new()
            .with_variable(
                "current_intent",
                json!(if current_intent.is_empty() {
                    "（无）"
                } else {
                    current_intent
                }),
            )
            .with_variable(
                "next_intent",
                json!(if next_intent.is_empty() {
                    "（无）"
                } else {
                    next_intent
                }),
            )
            .with_variable("head_sample", json!(head_sample));
        let prompt = self
            .prompt_manager
            .render("planning/html_clean_format", &prompt_context)
            .await?;

        let request = ChatCompletionRequest {
            model: self.ai.model_name().to_string(),
            messages: vec![Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text { text: prompt }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            }],
            tools: None,
            temperature: Some(0.0),
            max_tokens: Some(64),
            stream: false,
            extra: Default::default(),
        };

        let response = self.ai.chat_completion(request).await?;
        let text = response
            .choices
            .first()
            .and_then(|c| c.message.content.as_ref())
            .and_then(|c| match c {
                MessageContent::Text { text } => Some(text.clone()),
                _ => None,
            })
            .ok_or_else(|| anyhow!("Format decider returned no content"))?;

        let parsed: FormatChoice = self
            .prompt_manager
            .parse_response("planning/html_clean_format", &text)
            .await?;

        let fmt = parsed.format.to_lowercase();
        match fmt.as_str() {
            "text" | "markdown" => Ok(fmt),
            other => Err(anyhow!("Unknown format '{other}' from decider")),
        }
    }
}

/// HTML 清洗子 Agent。
///
/// 接收原始 HTML、当前意图和下一步意图，决定使用 `text` 或 `markdown`，
/// 在内部执行 `builtin_clean_html`，并且只返回清洗后的 `content`。
/// 原始 HTML 不会进入主 ReAct 上下文，主上下文只能收到清洗后的内容。
pub struct HtmlCleanSubAgent {
    decider: Arc<dyn FormatDecider>,
    tool_registry: Arc<ToolRegistry>,
}

impl HtmlCleanSubAgent {
    pub fn new(decider: Arc<dyn FormatDecider>, tool_registry: Arc<ToolRegistry>) -> Self {
        Self { decider, tool_registry }
    }

    /// 便捷构造函数，接入基于 LLM 和 PromptManager 的格式决策器。
    pub fn with_llm_decider<PM: PromptManager + 'static>(
        ai: Arc<dyn AiClient>,
        prompt_manager: Arc<PM>,
        tool_registry: Arc<ToolRegistry>,
    ) -> Self {
        Self::new(
            Arc::new(LlmFormatDecider::new(ai, prompt_manager)),
            tool_registry,
        )
    }

    /// 执行完整清洗流程，仅返回清洗后的 `content` 字符串。
    pub async fn process(
        &self,
        raw_html: &str,
        current_intent: &str,
        next_intent: &str,
    ) -> Result<String> {
        // 第一步：使用低成本启发式规则判断是否为 HTML，不调用 LLM。
        if !looks_like_html(raw_html) {
            return Err(anyhow!("input is not HTML"));
        }

        // 第二步：通过小型 LLM 调用选择输出格式。
        let head_sample: String = raw_html.chars().take(2048).collect();
        let format = match self
            .decider
            .decide(&head_sample, current_intent, next_intent)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    "HTML clean format decider failed, falling back to 'text': {}",
                    e
                );
                "text".to_string()
            }
        };

        // 第三步：将完整原始 HTML 传给本地内置清洗工具。
        let outcome: ToolOutcome = self
            .tool_registry
            .call_tool(
                "builtin_clean_html",
                json!({ "html": raw_html, "format": format }),
            )
            .await?;

        let content = outcome.result.content["content"]
            .as_str()
            .ok_or_else(|| anyhow!("cleaner returned no content"))?
            .to_string();

        Ok(content)
    }
}

/// 使用启发式规则检测 HTML，不调用 LLM；无法确定时返回 `false`。
pub fn looks_like_html(s: &str) -> bool {
    let trimmed = s.trim_start();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed.starts_with("<!doctype") || trimmed.starts_with("<!DOCTYPE") {
        return true;
    }
    if trimmed.starts_with("<?xml") {
        return true;
    }
    if trimmed.starts_with("<html") || trimmed.starts_with("<HTML") {
        return true;
    }

    if trimmed.len() < 64 {
        return false;
    }

    // 在较短的头部样本中检查标签密度和常见标签。
    let head: String = trimmed.chars().take(512).collect();
    let opens = head.matches('<').count();
    let closes = head.matches('>').count();
    if opens < 3 || closes < 3 {
        return false;
    }

    const COMMON_TAGS: &[&str] = &[
        "<div", "<DIV", "<p ", "<P ", "<p>", "<P>", "<a ", "<A ", "<a>", "<A>",
        "<span", "<SPAN", "<article", "<ARTICLE", "<section", "<SECTION",
        "<header", "<HEADER", "<footer", "<FOOTER", "<nav", "<NAV",
        "<table", "<TABLE", "<ul", "<UL", "<ol", "<OL", "<li", "<LI",
        "<h1", "<h2", "<h3", "<h4", "<h5", "<h6",
        "<img", "<IMG", "<main", "<MAIN", "<body", "<BODY", "<head", "<HEAD",
    ];
    COMMON_TAGS.iter().any(|t| head.contains(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_html_doctype() {
        assert!(looks_like_html("<!doctype html><html><body>hi</body></html>"));
    }

    #[test]
    fn looks_like_html_article() {
        assert!(looks_like_html(
            "<article><p>Some content with multiple elements and tags</p></article>"
        ));
    }

    #[test]
    fn looks_like_html_negative_text() {
        assert!(!looks_like_html("just a plain sentence without markup at all"));
    }

    #[test]
    fn looks_like_html_negative_short() {
        assert!(!looks_like_html("<p>tiny</p>"));
    }
}
