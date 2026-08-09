//! 待处理的 UI 交互状态——Agent 通过 `request_user_action` tool 请求用户操作。

use planned_agent::chat::UIAction;
use planned_agent_core::ai::types::Message;

use super::WorkflowPhase;

/// 待处理的 UI 交互状态——Agent 通过 `request_user_action` tool 请求用户操作。
///
/// 字段以 `pub(crate)` 暴露给 plan 模块内构造与读取。
#[derive(Clone)]
pub(crate) struct PendingUIState {
    /// 展示给用户的引导文本
    pub(crate) message: String,
    /// 用户可选的动作列表
    pub(crate) actions: Vec<UIAction>,
    /// 当时的对话历史快照（用于用户操作后继续 chat）
    pub(crate) history_snapshot: Vec<Message>,
    /// 触发该 pending 的工作流阶段（用于用户操作后确定下一步）
    pub(crate) trigger_phase: WorkflowPhase,
}

impl PartialEq for PendingUIState {
    fn eq(&self, other: &Self) -> bool {
        // history_snapshot 仅用于恢复对话，不参与渲染 diff
        self.message == other.message
            && self.actions == other.actions
            && self.trigger_phase == other.trigger_phase
    }
}
