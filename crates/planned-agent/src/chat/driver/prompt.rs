//! system prompt 注入逻辑。
//!
//! 渲染 `ChatConfig::system_prompt_template` 指定的模板并压入历史首部；
//! 若历史首条已是 System 则跳过（幂等，保证 system prompt 只注入一次）。
//!
//! v2 内部维护 history，system prompt 只在首次 `send` 时注入一次；
//! 后续 `send` 保留（历史首条已是 System 时不再重复注入）。

use anyhow::{anyhow, Result};
use planned_agent_core::prompt::{PromptContext, PromptManager};
use serde_json::Value;
use tracing::info;

use crate::chat::state::State;

/// 渲染并注入 system prompt 到历史首部（幂等：历史首条已是 System 则跳过）。
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