//! flexible_step2 结果回调：子 agent 完成后解析/提取最终 tool result。
//!
//! 纯通知，不改变 tool result 本身。可用于：
//! - 解析任务执行结果（提取结构化数据、轨迹摘要等）
//! - 收集到外部存储供业务侧消费
//! - 日志/埋点

use std::sync::Arc;

use planned_agent::chat::{ResultDecision, SubAgentResultCallback};
use planned_agent_core::mcp::types::ToolResult;

/// flexible_step2 结果回调。
///
/// 子 agent 完成后，`on_result` 被调用，
/// `result.content` 为 `extract_last_assistant_text` 的文本（即子 agent 最终输出）。
pub struct FlexibleStep2Callback;

impl SubAgentResultCallback for FlexibleStep2Callback {
    fn on_result(&self, agent_name: &str, result: &ToolResult) -> ResultDecision {
        tracing::info!(
            "[flexible_step2] 子 agent '{}' 完成, content_len={}, is_error={}",
            agent_name,
            result.content.as_str().map(|s| s.len()).unwrap_or(0),
            result.is_error,
        );
        // TODO: 解析 result.content，提取任务执行结果/轨迹摘要等
        //
        // 返回值：
        // - ResultDecision::Accept                → 接受原始结果
        // - ResultDecision::Transform(new_text)   → 替换 content
        // - ResultDecision::Retry(correction_msg) → 发送纠正消息给子 agent，重试
        //
        // 示例：
        // let text = result.content.as_str().unwrap_or("");
        // match serde_json::from_str::<serde_json::Value>(text) {
        //     Ok(_) => ResultDecision::Accept,
        //     Err(_) => ResultDecision::Retry(
        //         "输出格式错误，请严格输出 JSON 格式。".to_string()
        //     ),
        // }

        ResultDecision::Accept
    }
}

/// 创建 `flexible_step2` 回调实例（方便传给 `register_sub_agent`）。
pub fn create_step2_callback() -> Option<Arc<dyn SubAgentResultCallback>> {
    Some(Arc::new(FlexibleStep2Callback))
}
