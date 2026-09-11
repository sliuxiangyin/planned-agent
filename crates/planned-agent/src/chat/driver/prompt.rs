//! system prompt 注入逻辑。

use anyhow::{anyhow, Result};
use planned_agent_core::prompt::{PromptContext, PromptManager};
use tracing::info;

use crate::chat::service::SystemPrompt;
use crate::chat::state::State;

pub(super) async fn inject_system_prompt<PM: PromptManager + Send + Sync + 'static>(
    state: &State<PM>,
) -> Result<()> {
    let system_prompt = {
        let cfg = state.config.lock().unwrap();
        cfg.system_prompt.clone()
    };
    let Some(system_prompt) = system_prompt else {
        return Ok(());
    };
    if state.history.first_is_system() {
        return Ok(());
    }
    match system_prompt {
        SystemPrompt::Rendered(rendered) => {
            state.history.push_front_system(rendered);
            info!("chat: Injected pre-rendered system prompt");
        }
        SystemPrompt::Template(template) => {
            let rendered = state
                .prompt_manager
                .render(&template, &PromptContext::new())
                .await
                .map_err(|e| anyhow!("system prompt 模板 '{}' 渲染失败: {}", template, e))?;
            state.history.push_front_system(rendered);
            info!("chat: Injected system prompt from template '{}'", template);
        }
    }
    Ok(())
}
