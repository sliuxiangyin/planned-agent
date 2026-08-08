//! 加载计划数据：元数据 + 历史消息（周密）或 plans_flexible 快照（灵活）。
//!
//! 灵活模式不再加载 messages 表，改为从 plans_flexible 读取四字段构造 context。

use std::sync::Arc;

use crate::storage::repository::{MessageRepo, PlanFlexibleRepo, PlanRepo};
use dioxus::prelude::*;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use super::super::states::{ChatState, PlanState, WorkflowState};
use super::super::types::{PlanFlexibleSnapshot, PlanInfo};

/// 从 DB 异步加载计划元数据。
/// - 周密模式：同时加载历史消息
/// - 灵活模式：同时加载最新 plans_flexible 快照，不加载 messages
pub async fn load_plan_data(
    pid: String,
    plan_repo: Arc<PlanRepo>,
    msg_repo: Arc<MessageRepo>,
    flex_repo: Option<Arc<PlanFlexibleRepo>>,
    mut chat: ChatState,
    mut plan: PlanState,
    mut workflow: Option<WorkflowState>,
) {
    // ── 加载计划元数据 ──
    if let Ok(Some(plan_model)) = plan_repo.find_by_id(&pid).await {
        tracing::info!(
            "load_plan_data: 加载计划 '{}', mode='{}', status='{}'",
            plan_model.name,
            plan_model.mode,
            plan_model.status,
        );
        plan.plan_info.set(Some(PlanInfo {
            name: plan_model.name,
            mode: plan_model.mode.clone(),
            status: plan_model.status,
            created_at: plan_model.created_at,
        }));
        plan.set_mode(plan_model.mode);
    }

    // ── 按模式加载数据 ──
    let mode = plan.mode();
    if mode == "flexible" {
        // 灵活模式：加载 plans_flexible 快照，不加载 messages
        if let (Some(flex_repo), Some(mut wf)) = (flex_repo, workflow) {
            if let Ok(Some(snapshot)) = flex_repo.find_latest(&pid).await {
                let ctx = PlanFlexibleSnapshot {
                    version: snapshot.version as i64,
                    todos: snapshot.todos,
                    previous_summary: snapshot.previous_summary,
                    params: snapshot.params,
                    output_schema: snapshot.output_schema,
                    input_schema: snapshot.input_schema,
                };
                tracing::info!(
                    "load_plan_data: 加载灵活模式快照 v{}, has_todos={}, has_summary={}, has_params={}, has_schema={}, has_input_schema={}",
                    ctx.version,
                    !ctx.todos.is_empty(),
                    !ctx.previous_summary.is_empty(),
                    !ctx.params.is_empty(),
                    !ctx.output_schema.is_empty(),
                    !ctx.input_schema.is_empty(),
                );
                wf.context_snapshot.set(Some(ctx));
            } else {
                tracing::info!("load_plan_data: 灵活模式无历史快照，显示首次执行界面");
                wf.context_snapshot.set(None);
            }
        }
    } else {
        // 周密模式：加载历史消息
        if let Ok(msg_list) = msg_repo.find_by_plan_id(&pid).await {
            let loaded: Vec<Message> = msg_list
                .into_iter()
                .map(|m| Message {
                    role: match m.role.as_str() {
                        "user" => MessageRole::User,
                        "assistant" => MessageRole::Assistant,
                        "system" => MessageRole::System,
                        "tool" => MessageRole::Tool,
                        _ => MessageRole::User,
                    },
                    content: if m.content.is_empty() {
                        None
                    } else {
                        Some(MessageContent::Text { text: m.content })
                    },
                    ..Default::default()
                })
                .collect();
            chat.messages.set(loaded);
            chat.reasoning_texts
                .set(vec![None; chat.messages.read().len()]);
        }
    }
}

/// 从 PlanFlexibleSnapshot 构造注入到 prompt 的 context 字符串。
///
/// 格式化为 Markdown 供 `flexible_system.toml` 和 `flexible_clarity.toml`
/// 的 `{{ context }}` 变量使用。
pub fn build_context_string(snapshot: &PlanFlexibleSnapshot) -> String {
    if !snapshot.has_data() {
        return String::new();
    }

    let mut ctx = format!("## 历史执行计划（v{}）\n\n", snapshot.version);

    // TODO 步骤
    if !snapshot.todos.is_empty() {
        ctx.push_str("### 执行步骤（todos）\n");
        // todos 是 CoarseGrainedPlan JSON，格式化为可读文本
        ctx.push_str(&format_todos(&snapshot.todos));
        ctx.push('\n');
    }

    // 执行总结
    if !snapshot.previous_summary.is_empty() {
        ctx.push_str("### 上次执行总结（previous_summary）\n");
        ctx.push_str(&snapshot.previous_summary);
        ctx.push('\n');
    }

    // 参数定义
    if !snapshot.params.is_empty() && snapshot.params != "[]" {
        ctx.push_str("### 参数定义（params）\n");
        ctx.push_str(&format_params(&snapshot.params));
        ctx.push('\n');
    }

    // 输出格式
    if !snapshot.output_schema.is_empty() {
        ctx.push_str("### 输出格式（output_schema）\n");
        ctx.push_str(&snapshot.output_schema);
        ctx.push('\n');
    } else {
        ctx.push_str("### 输出格式（output_schema）\n（未记录）\n\n");
    }

    // 输入参数定义
    if !snapshot.input_schema.is_empty() {
        ctx.push_str("### 输入参数定义（input_schema）\n");
        ctx.push_str(&format_input_schema(&snapshot.input_schema));
        ctx.push('\n');
    }

    ctx
}

/// 将 CoarseGrainedPlan JSON 中的 todos 格式化为可读文本。
fn format_todos(todos_json: &str) -> String {
    // 尝试解析 JSON 并提取 step intent
    match serde_json::from_str::<serde_json::Value>(todos_json) {
        Ok(json) => {
            if let Some(steps) = json.get("steps").and_then(|s| s.as_array()) {
                steps
                    .iter()
                    .enumerate()
                    .map(|(i, step)| {
                        let intent = step
                            .get("intent")
                            .and_then(|v| v.as_str())
                            .unwrap_or("(无描述)");
                        format!("{}. {}\n", i + 1, intent)
                    })
                    .collect::<Vec<_>>()
                    .join("")
            } else {
                // 无法解析结构，原样返回
                todos_json.to_string()
            }
        }
        Err(_) => todos_json.to_string(),
    }
}

/// 将 ParamDef JSON 格式化为可读文本。
fn format_params(params_json: &str) -> String {
    match serde_json::from_str::<Vec<serde_json::Value>>(params_json) {
        Ok(params) => {
            params
                .iter()
                .map(|p| {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let desc = p
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let example = p.get("example").and_then(|v| v.as_str()).unwrap_or("");
                    if example.is_empty() {
                        format!("- {}: {}\n", name, desc)
                    } else {
                        format!("- {}: {}（示例: {}）\n", name, desc, example)
                    }
                })
                .collect::<Vec<_>>()
                .join("")
        }
        Err(_) => params_json.to_string(),
    }
}

/// 将 input_schema JSON 对象格式化为可读文本。
///
/// input_schema 形如 `{"keyword": {"type": "string", "description": "搜索关键词", "example": "安仁乡"}}`，
/// 逐参数渲染为 `- keyword (string): 搜索关键词（示例: 安仁乡）`。
fn format_input_schema(input_schema_json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(input_schema_json) {
        Ok(json) if json.is_object() => {
            let obj = json.as_object().unwrap();
            if obj.is_empty() {
                return "(空)".to_string();
            }
            let mut lines: Vec<String> = obj
                .iter()
                .map(|(name, def)| {
                    let param_type = def
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let desc = def
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let example = def
                        .get("example")
                        .map(|v| {
                            if v.is_string() {
                                v.as_str().unwrap_or("").to_string()
                            } else {
                                v.to_string()
                            }
                        })
                        .unwrap_or_default();
                    if example.is_empty() {
                        format!("- {} ({}): {}", name, param_type, desc)
                    } else {
                        format!("- {} ({}): {}（示例: {}）", name, param_type, desc, example)
                    }
                })
                .collect();
            lines.sort();
            lines.join("\n")
        }
        _ => input_schema_json.to_string(),
    }
}
