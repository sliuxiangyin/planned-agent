//! Plan 模块的内部类型与 `Message` 辅助函数。
//!
//! 仅 `plan` 子模块内部使用，所有项以 `pub(super)` 暴露给同级模块。

use planned_agent_core::types::{Message, MessageContent, MessageRole, UIAction};

/// 从 `core::Message` 取出可显示文本（仅 `MessageContent::Text`；其他变体视为空串）
pub(super) fn display_text(msg: &Message) -> &str {
    match &msg.content {
        Some(MessageContent::Text { text }) => text.as_str(),
        _ => "",
    }
}

/// 取可变文本引用（用于流式追加 chunk；非 `Text` 变体返回 `None`）
pub(super) fn display_text_mut(msg: &mut Message) -> Option<&mut String> {
    match &mut msg.content {
        Some(MessageContent::Text { text }) => Some(text),
        _ => None,
    }
}

/// `MessageRole` → UI CSS class（仅 GUI 层使用，避免在 core 上加 UI 关注点）
pub(super) fn role_css_class(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Tool => "tool",
    }
}

/// 待处理的 UI 交互状态——Agent 通过 `request_user_action` tool 请求用户操作。
///
/// 字段以 `pub(super)` 暴露给同级 `chat` 模块构造与读取。
#[derive(Clone)]
pub(super) struct PendingUIState {
    /// 展示给用户的引导文本
    pub(super) message: String,
    /// 用户可选的动作列表
    pub(super) actions: Vec<UIAction>,
    /// 当时的对话历史快照（用于用户操作后继续 chat）
    pub(super) history_snapshot: Vec<Message>,
}
