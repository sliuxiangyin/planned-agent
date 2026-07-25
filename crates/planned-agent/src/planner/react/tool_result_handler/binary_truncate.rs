//! 大输出截断 handler
//!
//! 当 `Observation.output` 序列化后超过 `max_bytes` 字节时，截断字符串并附加截断标记，
//! 防止超大输出（如 base64 图片、巨型日志）撑爆主 LLM 上下文。

use async_trait::async_trait;
use planned_agent_core::planner::react::Observation;
use serde_json::Value;

use crate::planner::react::tool_result_router::ObservationPostHandler;

/// 大输出截断后处理器。
///
/// 由 router 决定何时触发（`PostProcessKind::BinaryTruncate`），本 handler 只做"截断执行"。
/// 注：handler trait 只接收 `obs / current_intent / next_intent` 三参数。
pub struct BinaryTruncatePostHandler {
    /// 超过该字节数则截断
    max_bytes: usize,
}

impl BinaryTruncatePostHandler {
    pub fn new(max_bytes: usize) -> Self {
        Self { max_bytes }
    }
}

#[async_trait]
impl ObservationPostHandler for BinaryTruncatePostHandler {
    async fn handle(
        &self,
        obs: Observation,
        _current_intent: &str,
        _next_intent: &str,
    ) -> Observation {
        // 错误直接透传（不需要截断）
        if obs.error.is_some() {
            return obs;
        }

        // 优先按"原始字符串"处理（避免双重 JSON 序列化引入引号噪音）
        // 非字符串类型才走 JSON 序列化路径
        let (original_str, original_len) = match obs.output.as_str() {
            Some(s) => (s.to_string(), s.len()),
            None => {
                let s = serde_json::to_string(&obs.output).unwrap_or_default();
                let len = s.len();
                (s, len)
            }
        };

        if original_len <= self.max_bytes {
            return obs;
        }

        let truncated = truncate_string(&original_str, self.max_bytes);
        let marker = format!(
            "\n\n[...已截断，原输出 {} 字节，仅保留前 {} 字节...]",
            original_len,
            self.max_bytes
        );
        let mut new_output = String::with_capacity(truncated.len() + marker.len());
        new_output.push_str(&truncated);
        new_output.push_str(&marker);

        Observation {
            output: Value::String(new_output),
            is_complete: obs.is_complete,
            error: obs.error,
            duration_ms: obs.duration_ms,
        }
    }
}

/// 按 UTF-8 字节数截断字符串，保证不切到多字节字符中间。
fn truncate_string(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    // 确保不切到 UTF-8 多字节字符中间
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obs_with_output(value: Value) -> Observation {
        Observation {
            output: value,
            is_complete: false,
            error: None,
            duration_ms: 0,
        }
    }

    #[tokio::test]
    async fn short_output_is_not_modified() {
        let h = BinaryTruncatePostHandler::new(100);
        let obs = obs_with_output(json!("short text"));
        let result = h.handle(obs, "c", "n").await;
        assert_eq!(result.output.as_str().unwrap(), "short text");
    }

    #[tokio::test]
    async fn long_string_is_truncated() {
        let h = BinaryTruncatePostHandler::new(10);
        let big = "x".repeat(100);
        let result = h.handle(obs_with_output(json!(big)), "c", "n").await;
        let out = result.output.as_str().unwrap();
        assert!(out.starts_with('x'), "应以截断内容开头");
        assert!(out.contains("已截断"), "应包含截断标记");
        assert!(out.contains("原输出 100 字节"), "标记应说明原大小");
    }

    #[tokio::test]
    async fn long_json_object_is_truncated_by_serialized_size() {
        let h = BinaryTruncatePostHandler::new(50);
        let obj = json!({"data": "x".repeat(200)});
        let result = h.handle(obs_with_output(obj), "c", "n").await;
        let out = result.output.as_str().unwrap();
        assert!(out.contains("已截断"));
    }

    #[tokio::test]
    async fn utf8_boundary_is_respected() {
        let h = BinaryTruncatePostHandler::new(5);
        let s = "中文字符串"; // 每个汉字 3 字节
        let result = h.handle(obs_with_output(json!(s)), "c", "n").await;
        let out = result.output.as_str().unwrap();
        assert!(out.starts_with('中'), "应保留完整的'中'字");
        assert!(out.contains("已截断"));
    }

    #[tokio::test]
    async fn error_observation_passes_through() {
        let h = BinaryTruncatePostHandler::new(10);
        let mut obs = obs_with_output(json!("x".repeat(1000)));
        obs.error = Some("failed".into());
        let result = h.handle(obs, "c", "n").await;
        assert_eq!(result.error.as_deref(), Some("failed"));
        assert_eq!(
            result.output.as_str().unwrap().len(),
            1000,
            "error 输出不应被截断"
        );
    }
}