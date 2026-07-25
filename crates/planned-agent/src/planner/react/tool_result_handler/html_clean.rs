//! Browser 分类的 HTML 清洗 handler
//!
//! 当 `Observation.output` 是疑似 HTML 时，调用 [`HtmlCleanSubAgent`] 做结构化清洗，
//! 把清洗后的 `content` 字符串替换原 output。原始 HTML 永远不会进入主 ReAct 上下文。

use std::sync::Arc;
use tracing::warn;

use async_trait::async_trait;
use planned_agent_core::planner::react::Observation;
use serde_json::Value;

use crate::planner::react::sub_agents::html_clean_subagent::HtmlCleanSubAgent;
use crate::planner::react::tool_result_router::ObservationPostHandler;

/// Browser 分类工具的 HTML 清洗后处理器。
///
/// 由 router 决定何时触发（`PostProcessKind::HtmlClean`），本 handler 不再做内容嗅探，
/// 只做"被调用了就执行"的职责（错误 observation 透传作为防御兜底）。
///
/// 注：handler trait 只接收 `obs / current_intent / next_intent` 三参数，
/// `categories` 仅用于路由决策，不下沉到 handler。
pub struct HtmlBrowserPostHandler {
    sub_agent: Arc<HtmlCleanSubAgent>,
}

impl HtmlBrowserPostHandler {
    pub fn new(sub_agent: Arc<HtmlCleanSubAgent>) -> Self {
        Self { sub_agent }
    }
}

#[async_trait]
impl ObservationPostHandler for HtmlBrowserPostHandler {
    async fn handle(
        &self,
        obs: Observation,
        current_intent: &str,
        next_intent: &str,
    ) -> Observation {
        // 错误直接透传（清洗无意义）
        if obs.error.is_some() {
            return obs;
        }

        let Some(raw_html) = extract_top_level_html_string(&obs.output) else {
            return obs;
        };

        match self
            .sub_agent
            .process(&raw_html, current_intent, next_intent)
            .await
        {
            Ok(cleaned) => replace_top_level_output(obs, Value::String(cleaned)),
            Err(e) => {
                warn!(
                    "HtmlBrowserPostHandler failed; passing raw observation through: {}",
                    e
                );
                obs
            }
        }
    }
}

/// 尝试从 `Observation.output` 提取顶层 HTML 字符串。
/// 仅当输出为疑似 HTML 的 `Value::String` 时返回 `Some(html)`。
fn extract_top_level_html_string(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    if crate::planner::react::sub_agents::html_clean_subagent::looks_like_html(s) {
        Some(s.to_string())
    } else {
        None
    }
}

fn replace_top_level_output(mut obs: Observation, new_output: Value) -> Observation {
    obs.output = new_output;
    obs
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    use crate::planner::react::sub_agents::html_clean_subagent::{FormatDecider, HtmlCleanSubAgent};

    fn obs_with_output(value: Value) -> Observation {
        Observation {
            output: value,
            is_complete: false,
            error: None,
            duration_ms: 0,
        }
    }

    struct AlwaysTextDecider;
    #[async_trait]
    impl FormatDecider for AlwaysTextDecider {
        async fn decide(
            &self,
            _head: &str,
            _cur: &str,
            _next: &str,
        ) -> anyhow::Result<String> {
            Ok("text".to_string())
        }
    }

    #[tokio::test]
    async fn handler_cleans_browser_html() {
        let decider: Arc<dyn FormatDecider> = Arc::new(AlwaysTextDecider);
        let reg = Arc::new(planned_agent_tool_manager::ToolRegistry::new());
        reg.register_builtin_provider(
            &planned_agent_tool_manager::builtin::web_tools::WebToolsProvider,
        );
        let sub_agent = Arc::new(HtmlCleanSubAgent::new(decider, reg));
        let handler = HtmlBrowserPostHandler::new(sub_agent);

        let raw_html = concat!(
            "<!doctype html><html><body><article><h1>Title</h1>",
            "<p><a href=\"https://example.com\">Example link</a> body content ",
            "with enough characters for deterministic HTML detection.</p>",
            "</article></body></html>"
        );

        let obs = handler
            .handle(
                obs_with_output(json!(raw_html)),
                "extract article content",
                "summarize content",
            )
            .await;

        let content = obs.output.as_str().unwrap();
        assert!(content.contains("Example link"));
        assert!(!content.contains("<!doctype"));
        assert!(!content.contains("https://example.com"));
    }

    #[tokio::test]
    async fn handler_passes_through_non_html_output() {
        // router 已经判定过是否走 HtmlClean；这里只验证"非 HTML 输出不被改动"
        let decider: Arc<dyn FormatDecider> = Arc::new(AlwaysTextDecider);
        let reg = Arc::new(planned_agent_tool_manager::ToolRegistry::new());
        let sub_agent = Arc::new(HtmlCleanSubAgent::new(decider, reg));
        let handler = HtmlBrowserPostHandler::new(sub_agent);

        let obs = handler
            .handle(
                obs_with_output(json!("plain text content")),
                "cur",
                "next",
            )
            .await;

        assert_eq!(obs.output.as_str().unwrap(), "plain text content");
    }

    #[tokio::test]
    async fn handler_passes_through_error_observation() {
        let decider: Arc<dyn FormatDecider> = Arc::new(AlwaysTextDecider);
        let reg = Arc::new(planned_agent_tool_manager::ToolRegistry::new());
        let sub_agent = Arc::new(HtmlCleanSubAgent::new(decider, reg));
        let handler = HtmlBrowserPostHandler::new(sub_agent);

        let mut obs = obs_with_output(json!("some text"));
        obs.error = Some("tool failed".into());

        let result = handler.handle(obs, "cur", "next").await;
        assert_eq!(result.error.as_deref(), Some("tool failed"));
        assert_eq!(result.output.as_str().unwrap(), "some text");
    }
}