//! 子 agent 结果回调：trait 定义 + 决策枚举。

use planned_agent_core::mcp::types::ToolResult;

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
}

/// 子 agent 结果回调：完成后可获取最终 tool result（用于外部解析/提取）。
pub trait SubAgentResultCallback: Send + Sync {
    /// 子 agent 完成后触发。
    ///
    /// - `agent_name`：子 agent 工具名（如 `"flexible_step1"`）
    /// - `result`：最终 tool result（`content` 即为 `extract_last_assistant_text` 的文本）
    ///
    /// 返回 [`ResultDecision`]：
    /// - `Accept`：接受结果
    /// - `Transform(text)`：替换 content
    /// - `Retry(msg)`：发送纠正消息给子 agent，重试后再次回调
    fn on_result(&self, agent_name: &str, result: &ToolResult) -> ResultDecision;
}
