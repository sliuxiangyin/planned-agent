//! Plan 模块的状态容器：Signal 状态结构体与方法。
//!
//! 与 `types`（纯类型/数据模型）分离，仅 `plan` 子模块内部使用，
//! 所有项以 `pub(super)` 暴露给同级模块。

use dioxus::prelude::*;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use super::types::{display_text, display_text_mut, ParamDef, PendingUIState, PlanInfo};

// ── 分组状态结构体 ──

/// 聊天相关状态：消息列表、推理文本、流式生成、待处理 UI、输入框。
///
/// 所有字段均为 `Signal`（`Copy`），可直接传入闭包/异步块，无需 clone。
#[derive(Clone, Copy)]
pub(super) struct ChatState {
    pub messages: Signal<Vec<Message>, SyncStorage>,
    pub reasoning_texts: Signal<Vec<Option<String>>, SyncStorage>,
    pub streaming_idx: Signal<Option<usize>, SyncStorage>,
    pub pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
    pub input_text: Signal<String, SyncStorage>,
}

/// 计划元数据状态：模式、版本号、基本信息。
#[derive(Clone, Copy, PartialEq)]
pub(super) struct PlanState {
    pub plan_info: Signal<Option<PlanInfo>, SyncStorage>,
    pub plan_mode: Signal<Option<String>, SyncStorage>,
    pub plan_version: Signal<u32, SyncStorage>,
    /// 已固化的参数定义（清晰度检查 multi_select 勾选后暂存，确认生成时随事件落库）
    pub plan_params: Signal<Vec<ParamDef>, SyncStorage>,
}

// ── ChatState 方法 ──

impl ChatState {
    /// 获取当前 streaming 索引的快照值。
    pub fn sidx(&self) -> Option<usize> {
        *self.streaming_idx.read()
    }

    /// 获取指定索引消息的可显示文本。
    pub fn text_at(&self, idx: usize) -> Option<String> {
        self.messages
            .read()
            .get(idx)
            .map(|m| display_text(m).to_string())
    }

    /// 获取最后一条 Assistant 消息的索引。
    pub fn last_assistant_idx(&self) -> Option<usize> {
        self.messages
            .read()
            .iter()
            .rposition(|m| matches!(m.role, MessageRole::Assistant))
    }

    // ── 以下方法需要 `&mut self`（内部调用 Signal 的 write/set） ──

    /// 推入用户消息 + assistant 占位消息，同时对齐 `reasoning_texts`，返回 assistant 索引。
    pub fn push_user_turn(&mut self, user_text: String) -> usize {
        let user_msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text: user_text }),
            ..Default::default()
        };
        let asst_msg = Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text {
                text: String::new(),
            }),
            ..Default::default()
        };
        let asst_idx;
        {
            let mut msgs = self.messages.write();
            msgs.push(user_msg);
            self.reasoning_texts.write().push(None);
            asst_idx = msgs.len();
            msgs.push(asst_msg);
            self.reasoning_texts.write().push(Some(String::new()));
        }
        self.streaming_idx.set(Some(asst_idx));
        asst_idx
    }

    /// 向指定索引的消息追加文本（用于流式 delta）。
    pub fn append_text(&mut self, idx: usize, chunk: &str) {
        if let Some(msg) = self.messages.write().get_mut(idx) {
            if let Some(t) = display_text_mut(msg) {
                t.push_str(chunk);
            }
        }
    }

    /// 向当前 streaming 消息追加文本。
    pub fn append_streaming(&mut self, chunk: &str) {
        if let Some(idx) = self.sidx() {
            self.append_text(idx, chunk);
        }
    }

    /// 向指定索引追加推理文本。
    pub fn append_reasoning(&mut self, idx: usize, chunk: &str) {
        if let Some(Some(buf)) = self.reasoning_texts.write().get_mut(idx) {
            buf.push_str(chunk);
        }
    }

    /// 向当前 streaming 消息追加推理文本。
    pub fn append_streaming_reasoning(&mut self, chunk: &str) {
        if let Some(idx) = self.sidx() {
            self.append_reasoning(idx, chunk);
        }
    }

    /// 将指定索引的消息文本替换为最终内容，并停止 streaming。
    pub fn finalize_at(&mut self, idx: usize, content: &str) {
        if let Some(msg) = self.messages.write().get_mut(idx) {
            if let Some(t) = display_text_mut(msg) {
                *t = content.to_string();
            }
        }
        self.streaming_idx.set(None);
    }

    /// 停止 streaming（保留当前内容不变）。
    pub fn stop_streaming(&mut self) {
        self.streaming_idx.set(None);
    }

    /// 向最后一条 Assistant 消息追加文本。
    pub fn append_to_last_assistant(&mut self, text: &str) {
        if let Some(idx) = self.last_assistant_idx() {
            self.append_text(idx, text);
        }
    }

    /// 设置待处理 UI 状态。
    pub fn set_pending(&mut self, state: PendingUIState) {
        *self.pending_ui.write() = Some(state);
    }

    /// 清除待处理 UI 状态。
    pub fn clear_pending(&mut self) {
        self.pending_ui.set(None);
    }

    /// 清空全部消息与相关状态（内存级别）。
    pub fn clear(&mut self) {
        self.messages.set(vec![]);
        self.reasoning_texts.set(vec![]);
        self.streaming_idx.set(None);
        self.pending_ui.set(None);
    }
}

// ── PlanState 方法 ──

impl PlanState {
    /// 获取当前计划模式字符串（默认为空串）。
    pub fn mode(&self) -> String {
        self.plan_mode.read().clone().unwrap_or_default()
    }

    /// 设置计划模式。
    pub fn set_mode(&mut self, mode: String) {
        self.plan_mode.set(Some(mode));
    }

    /// 覆盖暂存的固化参数（每次清晰度检查确认后以最新勾选为准）。
    pub fn set_params(&mut self, params: Vec<ParamDef>) {
        self.plan_params.set(params);
    }
}