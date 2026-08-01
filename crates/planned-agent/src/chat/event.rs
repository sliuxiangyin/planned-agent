//! 流式聊天事件
//!
//! `ChatEvent` 是 `ChatService::chat_with_callback` 接收的回调参数类型,
//! 用于将多轮 assistant 响应、tool 调用过程以增量的方式
//! 实时下发给上层消费者。

use planned_agent_core::types::{Message, UIAction};
use serde_json::Value;

/// 流式聊天事件。
///
/// 一轮 assistant 响应通常对应以下顺序:
/// `RoundStart` → 若干 [`ChatEvent::TextDelta`] / [`ChatEvent::ReasoningDelta`]
/// / [`ChatEvent::ToolCallStart`] / [`ChatEvent::ToolCallArgsDelta`]
/// → `ToolCallComplete` → `RoundEnd`。
///
/// 若 assistant 触发了 tool 调用,则在 `RoundEnd` 之后会追加
/// [`ChatEvent::ToolExecuted`],之后开始下一轮。
///
/// **没有 `Done` 事件**——正常完成通过 `chat_with_callback` 返回 `Ok(ChatResponse)`,
/// 异常通过 `Err`。
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// 一轮 assistant 响应开始(用于上层标记进度)。
    RoundStart {
        /// 从 1 开始的轮次编号。
        round: usize,
    },
    /// 文本片段(普通回答内容)。
    TextDelta(String),
    /// 推理内容片段(思考模式下的思维链)。
    ReasoningDelta(String),
    /// 工具调用开始(id 与 name 已确定)。
    ///
    /// 仅在该 tool_call 首次携带 `id` 时发出一次。
    ToolCallStart {
        /// OpenAI 风格的 tool_call_id。
        id: String,
        /// 被调用工具的名称。
        name: String,
    },
    /// 工具调用参数片段(增量,按 `id` 区分)。
    ///
    /// 多个 `ArgsDelta` 拼接即为完整 JSON 字符串。
    ToolCallArgsDelta {
        /// 对应的 tool_call_id。
        id: String,
        /// 参数增量(原始字符串片段,非 JSON 解析后的 Value)。
        delta: String,
    },
    /// 工具调用结束(参数已累积完整并解析为 JSON Value)。
    ToolCallComplete {
        id: String,
        name: String,
        /// 已解析为 `serde_json::Value` 的完整参数。
        arguments: Value,
    },
    /// 工具已执行。
    ToolExecuted {
        id: String,
        name: String,
        /// `true` 表示工具执行失败或返回错误。
        is_error: bool,
        /// 工具输出内容(原始 `ToolResult.content`)。
        content: Value,
    },
    /// Agent 请求用户交互——前端应渲染对应 UI 组件（按钮/选项列表等）。
    ///
    /// 触发时机：tool_calls 中检测到 `request_user_action` 时发出。
    /// 此后 chat 循环中断，调用方需收集用户选择后重新调用 `chat_with_callback`。
    UIActionRequest {
        /// 展示给用户的引导文本
        message: String,
        /// 用户可执行的动作列表
        actions: Vec<UIAction>,
    },
    /// 一轮 assistant 消息已写入历史。
    RoundEnd {
        /// 完整构造的 assistant `Message`(含 tool_calls / reasoning_content)。
        message: Message,
    },
}
