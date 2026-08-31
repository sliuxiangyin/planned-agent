//! Turn 生命周期 —— 用户发送 / 占位气泡 / turn 结束。

use dioxus::prelude::*;

use super::signals::ChatSignals;
use super::types::Bubble;

/// 构造一个 streaming 的 assistant 占位气泡。
fn assistant_placeholder() -> Bubble {
    Bubble {
        is_assistant: true,
        text: String::new(),
        reasoning: String::new(),
        is_streaming: true,
        tool_calls: Vec::new(),
    }
}

impl ChatSignals {
    /// 用户发送：push user 气泡 + assistant 占位气泡到 `active`。
    pub fn push_user_turn(&mut self, user_text: String) {
        let mut active = self.active.write();
        active.push(Bubble {
            is_assistant: false,
            text: user_text,
            reasoning: String::new(),
            is_streaming: false,
            tool_calls: Vec::new(),
        });
        active.push(assistant_placeholder());
    }

    /// push 一个 streaming 的 assistant 占位气泡（`RoundStart` 幂等兜底）。
    pub fn push_assistant_placeholder(&mut self) {
        self.active.write().push(assistant_placeholder());
    }

    /// turn 结束：把 `active` 整组并入 `bubbles`。
    pub fn finish_turn(&mut self) {
        let mut active = self.active.write();
        self.bubbles.write().extend(active.drain(..));
    }
}
