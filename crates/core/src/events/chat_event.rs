//! 聊天流事件类型（核心协议层）
//!
//! `ChatEvent` 是聊天过程事件的统一协议类型，定义在 core 层供
//! `planned-agent`（聊天服务）与 `planned-agent-tool-manager`（子 agent
//! 过程流旁路）共享：子 agent 内部 `ChatService` 产生的事件经
//! `ToolStreamEvent` 携带 `Option<ChatEvent>` 类型化直传主旁路，
//! 不再经过字符串降级。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ai::types::Message;
use crate::events::UIAction;
use crate::tool_registry::types::ToolSource;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        /// 工具来源（SubAgent / Mcp / Custom / Builtin），由服务端查 metadata 填入。
        source: Option<ToolSource>,
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
    /// 触发时机：tool_calls 中检测到 `request_user_action` 时发出，
    /// 或子 agent 挂起（`awaiting_user_action`）时发出。
    /// 此后 chat 循环中断，调用方需收集用户选择后重新调用 `chat_with_callback`
    /// （子 agent 场景则调用 `ChatService::resume_sub_agent`）。
    UIActionRequest {
        /// 展示给用户的引导文本
        message: String,
        /// 用户可执行的动作列表
        actions: Vec<UIAction>,
        /// 子 agent 会话 ID：本事件源自子 agent 挂起时非 `None`，
        /// 调用方恢复时应携带 `session_id` + 用户选择调用 `resume_sub_agent`；
        /// 主 agent 自身 `request_user_action` 时为 `None`。
        session_id: Option<String>,
    },
    /// 子 agent 流式事件（TextDelta / ReasoningDelta / ToolCall* 等）。
    ///
    /// GUI 按 `tool_call_id` 路由到对应 `AgentView` 渲染实时流。
    /// `UIActionRequest` 不走此通道——它仍作为独立 `CoreChatEvent` 变体直接转发，
    /// 因为主 agent 的 GUI 需要直接处理交互卡片。
    SubChat {
        /// 产生该事件的子 agent 的 tool_call_id（父 agent 的 tool_calls 里对应的那个 id）。
        tool_call_id: String,
        /// 子 agent 内部事件（TextDelta / ReasoningDelta / ToolCall* 等，递归包装）。
        event: Box<ChatEvent>,
    },
    /// 一轮 assistant 消息已写入历史。
    RoundEnd {
        /// 完整构造的 assistant `Message`(含 tool_calls / reasoning_content)。
        message: Message,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::types::{MessageContent, MessageRole};
    use crate::events::UIActionType;
    use serde_json::json;

    fn sample_ui_action() -> UIAction {
        UIAction {
            id: "ok".to_string(),
            action_type: UIActionType::Confirm,
            label: "确认".to_string(),
            description: None,
            options: vec![],
            allow_custom: true,
        }
    }

    fn sample_message() -> Message {
        Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text {
                text: "结论".to_string(),
            }),
            ..Default::default()
        }
    }

    /// round-trip 辅助：序列化 → 反序列化 → 与原始事件一致（ChatEvent 无 PartialEq，用 Debug 比较）
    fn round_trip(ev: &ChatEvent) {
        let value = serde_json::to_value(ev).expect("序列化 ChatEvent 失败");
        let back: ChatEvent = serde_json::from_value(value).expect("反序列化 ChatEvent 失败");
        assert_eq!(
            format!("{:?}", back),
            format!("{:?}", ev),
            "round-trip 后事件不一致"
        );
    }

    /// 9 个 variant 全覆盖：重点验证含 Message / UIAction / Value 的复杂载荷。
    #[test]
    fn chat_event_round_trip_all_variants() {
        round_trip(&ChatEvent::RoundStart { round: 1 });
        round_trip(&ChatEvent::TextDelta("你好".to_string()));
        round_trip(&ChatEvent::ReasoningDelta("思考".to_string()));
        round_trip(&ChatEvent::ToolCallStart {
            id: "call_1".to_string(),
            name: "tool_a".to_string(),
            source: None,
        });
        round_trip(&ChatEvent::ToolCallArgsDelta {
            id: "call_1".to_string(),
            delta: r#"{"a":1}"#.to_string(),
        });
        round_trip(&ChatEvent::ToolCallComplete {
            id: "call_1".to_string(),
            name: "tool_a".to_string(),
            arguments: json!({ "a": 1 }),
        });
        round_trip(&ChatEvent::ToolExecuted {
            id: "call_1".to_string(),
            name: "tool_a".to_string(),
            is_error: false,
            content: json!({ "result": "ok" }),
        });
        round_trip(&ChatEvent::UIActionRequest {
            message: "请确认".to_string(),
            actions: vec![sample_ui_action()],
            session_id: Some("sid".to_string()),
        });
        round_trip(&ChatEvent::SubChat {
            tool_call_id: "call_sub".to_string(),
            event: Box::new(ChatEvent::TextDelta("子 agent 输出".to_string())),
        });
        round_trip(&ChatEvent::RoundEnd {
            message: sample_message(),
        });
    }
}
