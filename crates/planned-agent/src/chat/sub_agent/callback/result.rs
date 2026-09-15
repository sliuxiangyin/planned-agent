//! 子 agent **完成后**回调：决定最终 tool result 的内容。

use async_trait::async_trait;
use planned_agent_core::mcp::types::ToolResult;

use super::SubAgentCallContext;

/// 子 agent 结果处理决策。
///
/// 回调返回此枚举，决定子 agent 最终 tool result 的内容。
/// 决策结果会流入：父 agent 上下文（history）→ 父 agent 后续 LLM 调用 → GUI `ToolExecuted.content` → 持久化。
pub enum ResultDecision {
    /// 接受结果，原样返回。
    ///
    /// 父 agent 看到的是子 agent 原始输出（`extract_last_assistant_text` 的文本）。
    Accept,
    /// 处理后的结果：用 `new_text` 替换原始 content。
    ///
    /// 原始文本被丢弃，父 agent 及所有下游看到的都是你提供的 `new_text`。
    /// 典型场景：从子 agent 大段 Markdown 中提取 JSON 块、清理格式、摘要等。
    Transform(String),
    /// 拒绝结果，发送纠正消息给子 agent，要求重新生成。
    ///
    /// - `String` 作为新的 user 消息发送给子 agent（如「输出格式错误，请严格输出 JSON」）
    /// - 子 agent 重新生成后，回调会被再次触发（最多重试 2 次）
    /// - 重试耗尽或子 agent 失败时，自动兜底使用原始结果
    Retry(String),
    /// 中断本次子 agent 调用：**不再重试**，直接把 `String` 作为失败原因返回。
    ///
    /// 用于回调侧发生**不可重试的硬错误**（如流程状态写库失败、外部依赖不可用）——
    /// 重试也修不好，而静默 [`Accept`](Self::Accept) 又会让上层误以为成功。
    /// 返回后本次调用的 tool result 标记为 `is_error = true`、内容为 `String`，
    /// 子 agent 不会重新生成。
    ///
    /// 与 [`Retry`](Self::Retry) 的分工：`Retry` 是「模型输出不对，让它重做」；
    /// `Abort` 是「模型没错，但外部动作失败了，立刻收场并如实上报」。
    Abort(String),
}

/// 子 agent 结果回调：完成后可获取最终 tool result（用于外部解析/提取）。
///
/// `on_result` 为 **async**：它在触发方的异步上下文（`collect_until_outcome`）中被
/// `await`，回调实现因此可以直接 `await` 持久化等异步副作用，无需自行 `tokio::spawn`
/// （这是之前同步签名的权宜）。通过 [`async_trait`] 实现，返回的 future 需 `Send`。
#[async_trait]
pub trait SubAgentResultCallback: Send + Sync {
    /// 子 agent 完成后触发。
    ///
    /// - `ctx`：本次调用的上下文（工具名 / tool_call_id / 父 agent 传入的原始参数）
    /// - `result`：最终 tool result（`content` 即为 `extract_last_assistant_text` 的文本）
    ///
    /// 返回 [`ResultDecision`]：
    /// - `Accept`：接受结果
    /// - `Transform(text)`：替换 content
    /// - `Retry(msg)`：发送纠正消息给子 agent，重试后再次回调
    /// - `Abort(reason)`：中断本次调用，`reason` 作为失败结果返回（不重试）
    async fn on_result(&self, ctx: &SubAgentCallContext, result: &ToolResult) -> ResultDecision;
}
