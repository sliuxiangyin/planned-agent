//! 状态查询 —— `ChatSignals` 的只读查询方法。
//!
//! 与数据操作分离，便于定位「当前状态是什么」的逻辑。

use dioxus::prelude::ReadableExt;

use super::signals::ChatSignals;
use super::types::ToolCallPhase;

impl ChatSignals {
    /// 是否正在流式输出（`active` 中任一气泡 `is_streaming=true`）。
    pub fn is_streaming(&self) -> bool {
        self.active.read().iter().any(|b| b.is_streaming)
    }

    /// 是否有待处理的交互卡片（`request_user_action` 挂起中）。
    pub fn has_pending(&self) -> bool {
        self.pending_ui.read().is_some()
    }

    /// 是否有工具调用正在执行（Pending/Running）。
    ///
    /// 用于防止 `RoundEnd` 与 `RoundStart` 之间的空隙误启用发送按钮。
    pub fn has_active_tool_call(&self) -> bool {
        self.active.read().iter().any(|b| {
            b.tool_calls
                .iter()
                .any(|t| matches!(t.phase, ToolCallPhase::Pending | ToolCallPhase::Running))
        })
    }

    /// 综合判断是否处于忙碌状态（streaming / 交互卡片 / 工具调用进行中）。
    pub fn is_busy(&self) -> bool {
        self.is_streaming() || self.has_pending() || self.has_active_tool_call()
    }
}
