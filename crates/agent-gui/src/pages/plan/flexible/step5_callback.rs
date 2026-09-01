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
        let parsed: Option<Value> = result.content.as_str().and_then(|text| {
            // 优先整条解析；失败则从混有思考/解释文本的内容中提取最后一个 JSON 对象
            serde_json::from_str(text)
                .ok()
                .or_else(|| extract_json_object(text))
        });

        let json = match parsed {
            Some(json) if json.is_object() => json,
            Some(_) => {
                tracing::warn!(
                    "[flexible_step5] 子 agent '{}' 输出非 JSON 对象，请求重试",
                    agent_name
                );
                return ResultDecision::Retry(
                    "输出不是合法的 JSON 对象。请严格输出包含 input_schema、output、steps、execution_plan、metadata 五个顶层字段的纯 JSON 对象。"
                        .to_string(),
                );
            }
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

        // 校验 steps 与 execution_plan 必须存在、均为数组、长度一致且 step_id 一一对应
        let steps_ok = matches!(json.get("steps"), Some(Value::Array(_)));
        let plan_ok = matches!(json.get("execution_plan"), Some(Value::Array(_)));
        if !steps_ok || !plan_ok {
            tracing::warn!(
                "[flexible_step5] 子 agent '{}' 缺少 steps 或 execution_plan 数组，请求重试",
                agent_name
            );
            return ResultDecision::Retry(
                "模板缺少 steps 或 execution_plan 数组字段。请严格输出包含 input_schema、output、steps、execution_plan、metadata 五个顶层字段的完整模板 JSON。"
                    .to_string(),
            );
        }
        let steps_arr = json["steps"].as_array().unwrap();
        let plan_arr = json["execution_plan"].as_array().unwrap();
        let mismatched = steps_arr.len() != plan_arr.len()
            || steps_arr.iter().zip(plan_arr.iter()).any(|(s, p)| {
                s.get("id").and_then(Value::as_str) != p.get("step_id").and_then(Value::as_str)
            });
        if mismatched {
            tracing::warn!(
                "[flexible_step5] 子 agent '{}' 的 steps 与 execution_plan 不一一对应，请求重试",
                agent_name
            );
            return ResultDecision::Retry(
                "steps 与 execution_plan 必须长度相同且 step_id 一一对应（steps[i].id 必须等于 execution_plan[i].step_id）。请修正后重新生成。"
                    .to_string(),
            );
        }

        let input_schema = take_or_default(&json, "input_schema", "{}");
        let output = take_or_default(&json, "output", "{}");
        let steps = take_or_default(&json, "steps", "[]");
        let execution_plan = take_or_default(&json, "execution_plan", "[]");

        let plan_id2 = plan_id.clone();
        let service2 = service.clone();
        tokio::spawn(async move {
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
                Err(e) => tracing::error!("[flexible_step5] 写入 plans_flexible 失败: {}", e),
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

/// 从可能混有思考/解释文本的内容中提取最后一个完整的 JSON 对象。
///
/// 子 agent 若把分析文字与 JSON 混在同一条消息里（例如模型将 reasoning 并入
/// `content`，见 `ai-openai::client::convert_response`），`serde_json::from_str`
/// 会整体失败。此函数定位最后一个 `}`，向前做花括号配对，截取平衡的根对象后
/// 再解析，从而忽略前缀杂讯。
fn extract_json_object(text: &str) -> Option<Value> {
    let end = text.rfind('}')?;
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut start = None;
    let mut i = end;
    // 从最后一个 '}' 向前扫描，找到与之配对的根 '{'
    loop {
        match bytes[i] {
            b'}' => depth = depth.saturating_add(1),
            b'{' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    start = Some(i);
                    break;
                }
            }
            _ => {}
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    let start = start?;
    serde_json::from_str(&text[start..=end]).ok()
}

/// 创建 `flexible_step5` 回调实例（方便传给 `register_sub_agent`）。
pub fn create_step5_callback(
    plan_id: String,
    service: Arc<PlansFlexibleService>,
) -> Option<Arc<dyn SubAgentResultCallback>> {
    Some(Arc::new(FlexibleStep5Callback::new(plan_id, service)))
}
