//! Plan 模块的内部类型、分组状态结构体与 `Message` 辅助函数。
//!
//! 仅 `plan` 子模块内部使用，所有项以 `pub(super)` 暴露给同级模块。

use dioxus::prelude::*;
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

// ── 计划生成事件 ──

/// 固化的计划参数定义（来自清晰度检查阶段 multi_select 勾选）。
///
/// 序列化为 JSON 数组存入 `plans_flexible.params`，
/// 供下次执行时渲染参数输入表单与注入 context。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ParamDef {
    /// 参数名（MultiSelect 选项 id，如 "keyword"）
    pub name: String,
    /// 参数描述（选项 label 中 "=" 左侧，如 "搜索关键词"）
    pub description: String,
    /// 本次固化的示例值（选项 label 中 "=" 右侧，如 "安仁乡"）
    pub example: String,
}

/// 计划来源模式。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PlanSource {
    /// 灵活模式（`flexible_system.toml`）——执行轨迹总结
    Flexible,
    /// 周密模式（`thorough_system.toml`）——需求确认后生成
    Thorough,
}

/// 计划生成事件——用户点击"确认生成"时触发。
///
/// 由 `chat::handle_user_action` 写入，`PlanTodoView` 读取监听。
#[derive(Clone, Debug)]
pub(crate) struct PlanGeneratedEvent {
    /// LLM 输出的计划/总结文本
    pub plan_text: String,
    /// 来源 prompt 模式
    pub source: PlanSource,
    /// 已固化的参数定义（来自清晰度检查 multi_select 勾选）
    pub params: Vec<ParamDef>,
}

// ── 从 DB 加载的计划元数据 ──

/// 计划基本信息（从 `plans` 表加载）。
#[derive(Clone)]
pub(super) struct PlanInfo {
    pub(super) name: String,
    pub(super) mode: String,
    pub(super) status: String,
    pub(super) created_at: String,
}

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

/// 计划元数据状态：模式、生成事件、版本号、基本信息。
#[derive(Clone, Copy)]
pub(super) struct PlanState {
    pub plan_info: Signal<Option<PlanInfo>, SyncStorage>,
    pub plan_mode: Signal<Option<String>, SyncStorage>,
    pub plan_generated: Signal<Option<PlanGeneratedEvent>, SyncStorage>,
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

    /// 当前是否正在流式生成。
    pub fn is_streaming(&self) -> bool {
        self.streaming_idx.read().is_some()
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

    /// 追加一条消息，返回其索引（不会对齐 `reasoning_texts`）。
    fn push_message_raw(&mut self, msg: Message) -> usize {
        let mut msgs = self.messages.write();
        let idx = msgs.len();
        msgs.push(msg);
        idx
    }

    /// 推入用户消息 + assistant 占位消息，同时对齐 `reasoning_texts`，返回 assistant 索引。
    pub fn push_user_turn(&mut self, user_text: String) -> usize {
        let user_msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text {
                text: user_text,
            }),
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

    /// 推入一条 System 消息，可选同时标记为 streaming。
    pub fn push_system(&mut self, text: String, start_streaming: bool) -> usize {
        let msg = Message {
            role: MessageRole::System,
            content: Some(MessageContent::Text { text }),
            ..Default::default()
        };
        let idx = self.push_message_raw(msg);
        if start_streaming {
            self.streaming_idx.set(Some(idx));
        }
        idx
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

    /// 在最后一条 Assistant 消息末尾追加"分割线 + 标题"状态块，并以闪烁光标表示进行中。
    ///
    /// 返回状态块所在消息索引；若不存在 Assistant 消息则回退为新 System 消息。
    pub fn start_status_block(&mut self, title: &str) -> usize {
        let Some(idx) = self.last_assistant_idx() else {
            return self.push_system(format!("**{title}**"), true);
        };
        self.append_text(idx, &format!("\n\n---\n\n**{title}**\n\n"));
        self.streaming_idx.set(Some(idx));
        idx
    }

    /// 结束状态块：追加最终结果文本并停止闪烁光标。
    pub fn finish_status_block(&mut self, idx: usize, result: &str) {
        self.append_text(idx, result);
        self.stop_streaming();
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

    /// 发出计划生成事件（携带已暂存的固化参数）。
    pub fn emit_generated(&mut self, plan_text: String, source: PlanSource) {
        let params = self.plan_params.read().clone();
        self.plan_generated.set(Some(PlanGeneratedEvent {
            plan_text,
            source,
            params,
        }));
    }

    /// 覆盖暂存的固化参数（每次清晰度检查确认后以最新勾选为准）。
    pub fn set_params(&mut self, params: Vec<ParamDef>) {
        self.plan_params.set(params);
    }

    /// 递增计划版本号。
    pub fn bump_version(&mut self) {
        self.plan_version.with_mut(|v| *v += 1);
    }
}
