//! 灵活模式协调器的**旁路工具**集合（每工具一文件）。
//!
//! - `flexible_state`：读写当前会话的「流程中间状态」（当前阶段 + 各步骤产物）。
//!   - `action=load`：读取 `current_step` 与 `products`，供协调器在「用户指定步骤」时盘点
//!     目标步骤的前置产物是否齐备（齐备→直达；缺失→引导）。
//!   - `action=save`：在某一流程阶段「定稿」（如 step1 确认、step2 success、step3/4 用户
//!     确认）后登记该阶段产物，并把 `current_step` 推进到对应档位。
//! - `save_flexible_template`：登记某会话产出的模板快照（由父 agent 在 step5 产出后显式调用）。
//!
//! `session_id` 不再从会话 watch 槽动态读取（多会话并行时会串写），改由协调器从 system
//! prompt 中照抄、作为工具参数传入；executor 从 `arguments.session_id` 读取。`plan_id` 仍由
//! executor 构造时注入（per-plan，天然正确），不依赖协调器传入。

mod flexible_state;
mod save_flexible_template;

pub(crate) use flexible_state::{flexible_state_tool, FlexibleStateExecutor};
pub(crate) use save_flexible_template::{save_flexible_template, SaveTemplateExecutor};

use anyhow::Result;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::ToolResult;

/// 从工具参数读取 `session_id`；缺失 / 空 / 非字符串时返回可读错误。
///
/// `session_id` 由协调器从 system prompt 中照抄传入（system prompt 已在全局主 agent 中定义）。
fn read_session_id(arguments: &Value) -> std::result::Result<String, String> {
    match arguments.get("session_id").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => Ok(s.to_string()),
        _ => Err(
            "缺少 session_id：请原样照抄 system prompt 中本会话的 session ID，不得改写或省略"
                .to_string(),
        ),
    }
}

/// 构造一个 is_error 的 ToolResult。
fn error_result(msg: &str) -> Result<ToolResult> {
    Ok(ToolResult {
        call_id: String::new(),
        content: json!({ "error": msg }),
        is_error: true,
    })
}
