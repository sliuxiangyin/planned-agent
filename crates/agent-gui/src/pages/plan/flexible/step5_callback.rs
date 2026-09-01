//! flexible_step5 结果回调：解析子 agent 输出的 JSON 模板，写入 plans_flexible 表。
//!
//! step5 完成后，`on_result` 拿到最终输出（`extract_last_assistant_text` 文本，纯 JSON），
//! 解析出 `input_schema` / `output` / `steps` / `execution_plan`，通过
//! `PlansFlexibleService::write` 持久化到 plans_flexible（version 由 repo 自动递增）。
//!
//! 纯旁路持久化：回调返回 `Accept`，原始 JSON 原样流回父 agent / GUI，不影响展示。

use std::sync::Arc;

use planned_agent::chat::{ResultDecision, SubAgentResultCallback};
use planned_agent_core::mcp::types::ToolResult;
use serde_json::Value;

use crate::services::plans_flexible_service::PlansFlexibleService;

/// flexible_step5 结果回调。
pub struct FlexibleStep5Callback {
    plan_id: String,
    service: Arc<PlansFlexibleService>,
}

impl FlexibleStep5Callback {
    pub fn new(plan_id: String, service: Arc<PlansFlexibleService>) -> Self {
        Self { plan_id, service }
    }
}

impl SubAgentResultCallback for FlexibleStep5Callback {
    fn on_result(&self, agent_name: &str, result: &ToolResult) -> ResultDecision {
        let plan_id = self.plan_id.clone();
        let service = self.service.clone();

        // 解析输出 JSON。失败或非 JSON 时 Retry，让子 agent 重新生成严格 JSON。
        let parsed: Option<Value> = result
            .content
            .as_str()
            .and_then(|text| serde_json::from_str(text).ok());

        let json = match parsed {
            Some(json) => json,
            None => {
                tracing::warn!(
                    "[flexible_step5] 子 agent '{}' 输出非合法 JSON，请求重试",
                    agent_name
                );
                return ResultDecision::Retry(
                    "输出不是合法的 JSON 格式。请严格输出纯 JSON，不要包含 Markdown 代码块标记（```json）或任何解释文本。"
                        .to_string(),
                );
            }
        };

        // step5 异常分支可能输出 {"status":"error",...}，请求重试补齐输入
        if json.get("status").and_then(Value::as_str) == Some("error") {
            tracing::warn!(
                "[flexible_step5] 子 agent '{}' 输出错误状态，请求重试",
                agent_name
            );
            return ResultDecision::Retry(
                "输出中的 status 为 error：缺少必要输入数据。请基于 task_definition、execution_trace、field_selection_result、parameter_confirmation_result 补齐后再生成完整模板 JSON。"
                    .to_string(),
            );
        }

        let input_schema = take_or_default(&json, "input_schema", "{}");
        let output = take_or_default(&json, "output", "{}");
        let steps = take_or_default(&json, "steps", "[]");
        let execution_plan = take_or_default(&json, "execution_plan", "[]");

        let plan_id2 = plan_id.clone();
        let service2 = service.clone();
        dioxus::prelude::spawn(async move {
            match service2
                .write(&plan_id2, &input_schema, &output, &steps, &execution_plan)
                .await
            {
                Ok(model) => tracing::info!(
                    "[flexible_step5] 已写入 plans_flexible: id={}, plan_id={}, version={}",
                    model.id,
                    model.plan_id,
                    model.version
                ),
                Err(e) => tracing::error!(
                    "[flexible_step5] 写入 plans_flexible 失败: {}",
                    e
                ),
            }
        });

        // 原始结果原样返回，由父 agent / GUI 继续消费
        ResultDecision::Accept
    }
}

/// 从 JSON 中取指定字段；缺失时回退到 `default`（作为字符串存储）。
fn take_or_default(json: &Value, key: &str, default: &str) -> String {
    json.get(key)
        .map(Value::to_string)
        .unwrap_or_else(|| default.to_string())
}

/// 创建 `flexible_step5` 回调实例（方便传给 `register_sub_agent`）。
pub fn create_step5_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Option<Arc<dyn SubAgentResultCallback>> {
    Some(Arc::new(FlexibleStep5Callback::new(plan_id, service)))
}
