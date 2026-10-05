//! LLM 请求：超时、超时重试、取消即时。

use std::sync::Arc;
use std::time::Duration;

use planned_agent_core::ai::types::{ChatCompletionRequest, ChatCompletionResponse, MessageRole};
use planned_agent_core::ai::AiClient;
use tokio::sync::watch;

use super::super::executor::ExecutorConfig;

pub(crate) fn is_cancelled(cancel: Option<&watch::Receiver<bool>>) -> bool {
    cancel.map(|receiver| *receiver.borrow()).unwrap_or(false)
}

/// 等到取消信号置位。
///
/// `cancel` 为 `None`（或发送端已 drop）时**永久挂起** —— 否则 `select!` 的这个分支
/// 会立刻完成，把每次调用都误判成「已取消」。
pub(crate) async fn wait_cancel(cancel: Option<&watch::Receiver<bool>>) {
    let Some(receiver) = cancel else {
        std::future::pending::<()>().await;
        return;
    };
    let mut receiver = receiver.clone();
    if *receiver.borrow() {
        return;
    }
    while receiver.changed().await.is_ok() {
        if *receiver.borrow() {
            return;
        }
    }
    // 发送端被 drop：视为「永不取消」。
    std::future::pending::<()>().await;
}

/// 按配置给请求套超时；`None` = 不限制（直接等待）。
pub(crate) async fn with_timeout<F, T>(timeout: Option<Duration>, future: F) -> Result<T, tokio::time::error::Elapsed>
where
    F: std::future::Future<Output = T>,
{
    match timeout {
        Some(duration) => tokio::time::timeout(duration, future).await,
        None => Ok(future.await),
    }
}

/// 一次请求的**规模构成**（字符数）—— 用来回答「token 花在哪」。
///
/// `prompt_tokens` 是真值，但看不出构成；而不同区段的优化手段完全不同：
/// - `definitions_chars` 大 → **收窄每步的工具表**（每步固定开销，且全部步骤受益）；
/// - `system_chars` 大 → 精简 system 段（提示词 / 环境事实）；
/// - `tool_chars` 大 → 工具**返回**的结果在累积（轮数越多越大）→ 考虑落盘 / 摘要；
/// - `assistant_chars` 大 → 多轮思考文本在累积。
///
/// 口径是**字符数**（不是字节）：中文 1 字 ≈ 1 token、英文 4 字 ≈ 1 token，
/// 字节会把中文放大三倍、误导占比。只用于看**占比**，不是精确 token。
struct RequestShape {
    messages: usize,
    system_chars: usize,
    user_chars: usize,
    assistant_chars: usize,
    tool_chars: usize,
    definitions_chars: usize,
    tools: usize,
}

impl RequestShape {
    fn of(request: &ChatCompletionRequest) -> Self {
        let mut system_chars = 0usize;
        let mut user_chars = 0usize;
        let mut assistant_chars = 0usize;
        let mut tool_chars = 0usize;
        for message in &request.messages {
            let chars = json_chars(message);
            match message.role {
                MessageRole::System => system_chars += chars,
                MessageRole::User => user_chars += chars,
                MessageRole::Assistant => assistant_chars += chars,
                MessageRole::Tool => tool_chars += chars,
            }
        }
        Self {
            messages: request.messages.len(),
            system_chars,
            user_chars,
            assistant_chars,
            tool_chars,
            definitions_chars: request.tools.as_ref().map_or(0, |tools| json_chars(tools)),
            tools: request.tools.as_ref().map_or(0, |tools| tools.len()),
        }
    }

    /// 与响应里的**真值** token 一起打点：一眼看出各区段的占比。
    fn log(&self, step: usize, round: usize, response: &ChatCompletionResponse) {
        let (prompt_tokens, completion_tokens) = response
            .usage
            .as_ref()
            .map(|usage| (usage.prompt_tokens, usage.completion_tokens))
            .unwrap_or((0, 0));
        tracing::info!(
            step,
            round,
            messages = self.messages,
            tools = self.tools,
            definitions_chars = self.definitions_chars,
            system_chars = self.system_chars,
            user_chars = self.user_chars,
            assistant_chars = self.assistant_chars,
            tool_chars = self.tool_chars,
            prompt_tokens,
            completion_tokens,
            "LLM 请求构成（字符数 + 真值 token）"
        );
    }
}

/// 序列化后的字符数（含 JSON 包装 —— 那部分也确实发给 provider）。
fn json_chars<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_string(value)
        .map(|json| json.chars().count())
        .unwrap_or(0)
}

/// 调一次 LLM 请求：带**单次请求超时**与**超时重试**，且让取消能**立即**打断。
///
/// 返回 `Err(原因)` 时调用方直接结束该步 —— 「用户取消」与「调用失败」走同一条出口，
/// 区别只在原因文本。
///
/// ⚠️ 超时的语义是「**一次 `AiClient::chat_completion` 调用**的墙钟上限」：该调用在
/// `ai-openai` 内部还套了 3 次重试，所以这里的超时**包住的是整次调用**（含内层重试）。
/// 也正因如此，超时后的重试必须在**这一层**做 —— `timeout` 会把内层 future 一并丢掉，
/// 内层自己没有机会再重试。
pub(crate) async fn request_llm(
    ai: &Arc<dyn AiClient>,
    request: &ChatCompletionRequest,
    cfg: &ExecutorConfig,
    cancel: Option<&watch::Receiver<bool>>,
    step_index: usize,
    round: usize,
) -> Result<ChatCompletionResponse, String> {
    // 诊断：只在 INFO 启用时才求值（序列化整份请求不白做）。
    let shape = tracing::enabled!(tracing::Level::INFO).then(|| RequestShape::of(request));
    let attempts = cfg.llm_timeout_retries.saturating_add(1);
    for attempt in 1..=attempts {
        let outcome = tokio::select! {
            // 取消优先：请求进行中也能立刻退出。
            biased;
            _ = wait_cancel(cancel) => {
                tracing::info!(
                    step = step_index,
                    round,
                    "LLM 请求进行中收到取消，中止该步"
                );
                return Err("用户取消".to_string());
            }
            result = with_timeout(cfg.llm_timeout, ai.chat_completion(request.clone())) => result,
        };

        match outcome {
            Ok(Ok(response)) => {
                if let Some(shape) = &shape {
                    shape.log(step_index, round, &response);
                }
                return Ok(response);
            }
            Ok(Err(err)) => {
                // 失败原因必须同时进日志：报告里的原因只有 UI 看得到，事后查问题只能靠日志。
                // 内层（`ai-openai`）已按自己的策略重试过 —— 这里**不再叠加**，直接失败。
                tracing::error!(
                    step = step_index,
                    round,
                    error = %err,
                    "LLM 调用失败，该步失败"
                );
                return Err(format!("LLM 调用失败：{err}"));
            }
            Err(elapsed) => {
                if attempt >= attempts {
                    tracing::error!(
                        step = step_index,
                        round,
                        attempts,
                        "LLM 请求超时且已用尽重试次数，该步失败"
                    );
                    return Err(format!("LLM 请求超时（{elapsed}，已尝试 {attempts} 次）"));
                }
                tracing::warn!(
                    step = step_index,
                    round,
                    attempt,
                    attempts,
                    "LLM 请求超时，将重试"
                );
            }
        }
    }
    // `attempts = llm_timeout_retries + 1 >= 1`，循环不会走到这里。
    unreachable!("attempts 至少为 1")
}

