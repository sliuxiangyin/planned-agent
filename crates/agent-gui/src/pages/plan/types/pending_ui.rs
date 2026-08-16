//! 待处理的 UI 交互状态——Agent 通过 `request_user_action` tool 请求用户操作。

use planned_agent::UIAction;
use planned_agent_core::ai::types::Message;

/// 待处理的 UI 交互状态——Agent 通过 `request_user_action` tool 请求用户操作。
///
/// 字段以 `pub(crate)` 暴露给 plan 模块内构造与读取。
#[derive(Clone)]
pub(crate) struct PendingUIState {
    /// 展示给用户的引导文本
    pub(crate) message: String,
    /// 用户可选的动作列表
    pub(crate) actions: Vec<UIAction>,
    /// 触发 pending 时的对话历史快照（用户操作后用于继续 chat）
    pub(crate) history_snapshot: Vec<Message>,
}

impl PartialEq for PendingUIState {
    fn eq(&self, other: &Self) -> bool {
        // history_snapshot 仅用于恢复对话，不参与渲染 diff
        self.message == other.message && self.actions == other.actions
    }
}