//! `core::Message` 的显示辅助函数（GUI 层）。
//!
//! 这些函数只做"取文本/取 class"的包装，不持有状态；
//! `MessageRole` → CSS class 的映射放这里，避免在 core 上加 UI 关注点。

use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

/// 从 `core::Message` 取出可显示文本（仅 `MessageContent::Text`；其他变体视为空串）
pub(crate) fn display_text(msg: &Message) -> &str {
    match &msg.content {
        Some(MessageContent::Text { text }) => text.as_str(),
        _ => "",
    }
}

/// 取可变文本引用（用于流式追加 chunk；非 `Text` 变体返回 `None`）
pub(crate) fn display_text_mut(msg: &mut Message) -> Option<&mut String> {
    match &mut msg.content {
        Some(MessageContent::Text { text }) => Some(text),
        _ => None,
    }
}

/// `MessageRole` → UI CSS class（仅 GUI 层使用，避免在 core 上加 UI 关注点）
pub(crate) fn role_css_class(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Tool => "tool",
    }
}
