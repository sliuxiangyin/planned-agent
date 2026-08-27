//! system prompt 注入逻辑。

use anyhow::{anyhow, Result};
use planned_agent_core::prompt::{PromptContext, PromptManager};
use serde_json::Value;
use tracing::info;

use crate::chat::state::State;

pub(super) async fn inject_system_prompt<PM: PromptManager + Send + Sync + 'static>(
    state: &State<PM>,
) -> Result<()> {
    let (template, context) = {
        let cfg = state.config.lock().unwrap();
        (cfg.system_prompt_template.clone(), cfg.context.clone())
    };
    let Some(template) = template else {
        return Ok(());
    };
    if state.history.first_is_system() {
        return Ok(());
    }
    let rendered = state
        .prompt_manager
        .render(
            &template,
            &PromptContext::new().with_variable(
                "context",
                Value::String(context.unwrap_or_default()),
            ),
        )
        .await
        .map_err(|e| anyhow!("system prompt 模板 '{}' 渲染失败: {}", template, e))?;
    state.history.push_front_system(rendered);
    info!(
        "chat: Injected system prompt from template '{}'",
        template
    );
    Ok(())
}
