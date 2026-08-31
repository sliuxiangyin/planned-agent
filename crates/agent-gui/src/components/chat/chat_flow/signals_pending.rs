//! PendingUI 管理 + 子 Agent 事件攒入。

use dioxus::prelude::*;

use super::signals::ChatSignals;
use super::types::{AgentEvent, PendingUI, ToolCallPhase};

impl ChatSignals {
    pub fn set_pending(&mut self, state: PendingUI) {
        *self.pending_ui.write() = Some(state);
    }

    pub fn clear_pending(&mut self) {
        self.pending_ui.set(None);
    }

    /// 攒入子 agent 流式事件。
    pub fn push_agent_event(&mut self, tool_call_id: &str, event: AgentEvent) {
        if let Some(av) = self.agent_views.write().get_mut(tool_call_id) {
            av.events.push(event);
        }
    }

    /// 子 agent 完成/出错：更新 phase，停止 streaming。
    pub fn finish_agent_view(&mut self, tool_call_id: &str, phase: ToolCallPhase) {
        if let Some(av) = self.agent_views.write().get_mut(tool_call_id) {
            av.phase = phase;
            av.is_streaming = false;
        }
    }
}
