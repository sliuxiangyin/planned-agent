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

    /// 把一条服务端 `Error` 文本渲染为可见的 assistant 气泡（兜底展示）。
    ///
    /// 部分错误路径（如子 agent 内 `request_user_action` 参数损坏直接错误收尾）
    /// 后端不会补一条独立的 assistant 文本，GUI 若仅靠 `Error` 事件做清理，
    /// 用户会在界面上看到空白而无任何原因说明。此方法保证 Error 至少有一条
    /// 可见文本。若当前 turn 已有带内容的 assistant 气泡（后端已补文本），
    /// 则在末尾追加一条分隔说明，避免重复一整块。
    pub fn render_error(&mut self, text: &str) {
        let mut active = self.active.write();
        if let Some(b) = active.iter_mut().rfind(|b| b.is_assistant) {
            if b.text.trim().is_empty() {
                b.text = text.to_string();
            } else {
                b.text.push('\n');
                b.text.push_str(text);
            }
        } else {
            let mut b = assistant_placeholder();
            b.is_streaming = false;
            b.text = text.to_string();
            active.push(b);
        }
    }
}
