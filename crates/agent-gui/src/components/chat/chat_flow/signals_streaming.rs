//! 流式更新 —— 文本 / 推理追加、停止 streaming。

use dioxus::prelude::*;

use super::signals::ChatSignals;

impl ChatSignals {
    /// 追加文本到 `active` 内最后一条 streaming 气泡。
    pub fn append_streaming_text(&mut self, chunk: &str) {
        // rfind：始终追加到最新一条 streaming 气泡（多 streaming 并存时避免写错旧气泡）
        if let Some(b) = self.active.write().iter_mut().rfind(|b| b.is_streaming) {
            b.text.push_str(chunk);
        }
    }

    /// 追加推理内容到 `active` 内最后一条 streaming 气泡。
    pub fn append_streaming_reasoning(&mut self, chunk: &str) {
        if let Some(b) = self.active.write().iter_mut().rfind(|b| b.is_streaming) {
            b.reasoning.push_str(chunk);
        }
    }

    /// 停止 streaming（`active` 内全部气泡置 `is_streaming=false`）。
    pub fn stop_streaming(&mut self) {
        for b in self.active.write().iter_mut() {
            b.is_streaming = false;
        }
    }

    /// 追加文本到 `active` 内最后一条 assistant 气泡。
    pub fn append_to_last_assistant(&mut self, text: &str) {
        if let Some(b) = self.active.write().iter_mut().rfind(|b| b.is_assistant) {
            b.text.push_str(text);
        }
    }
}
