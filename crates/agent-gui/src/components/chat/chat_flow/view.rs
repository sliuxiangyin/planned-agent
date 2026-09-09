//! `ChatView` —— 聊天会话的不可变 UI 投影（单一数据源）。
//!
//! 目标：把会话状态（`bubbles` / `active` / `agent_views` / `pending_ui` /
//! `pending_tool_call_id`）收敛为**一个**可整体读写的值类型，配合 `reduce.rs`
//! 的纯 reducer 与 `bridge.rs` 的 `ChatBridge`（持有 `Signal<ChatView>`）
//! 完成「事件 → 视图」的翻译。
//!
//! 本文件只承载「值操作方法」（对 `&mut self` 直接改字段）与「只读查询方法」，
//! 不含任何 dioxus signal 依赖——`ChatView` 是纯值类型，可脱离 dioxus 单测。

use std::collections::HashMap;

use super::types::{AgentEvent, AgentViewData, Bubble, PendingUI, ToolCallPhase, ToolViewData};

/// 聊天会话的 UI 投影。
///
/// 不包含：
/// - `input_text`（输入框文本）——属组件局部 UI 态，由 `chat_panel` 自己维护；
/// - `subscription`（事件订阅 guard）——由 `ChatBridge` 结构体字段持有。
#[derive(Clone, PartialEq, Default)]
pub struct ChatView {
    /// 历史气泡（已完成的 turn，`finish_turn` 并入）。
    pub bubbles: Vec<Bubble>,
    /// 当前 turn 气泡组（`send` → `Done`，流式增量更新的活跃区）。
    pub active: Vec<Bubble>,
    /// 子 agent 流式数据（key = tool_call_id）。
    pub agent_views: HashMap<String, AgentViewData>,
    /// 待处理的 UI 交互卡片（`request_user_action` / 子 agent 挂起）。
    pub pending_ui: Option<PendingUI>,
    /// 最近一次 `request_user_action` 的 tool_call_id（用于回填确认）。
    pub pending_tool_call_id: Option<String>,
}

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

// ── turn 生命周期 ──────────────────────────────────────

impl ChatView {
    /// 用户发送：push user 气泡 + assistant 占位气泡到 `active`。
    pub fn push_user_turn(&mut self, user_text: String) {
        self.active.push(Bubble {
            is_assistant: false,
            text: user_text,
            reasoning: String::new(),
            is_streaming: false,
            tool_calls: Vec::new(),
        });
        self.active.push(assistant_placeholder());
    }

    /// push 一个 streaming 的 assistant 占位气泡（`RoundStart` 幂等兜底）。
    pub fn push_assistant_placeholder(&mut self) {
        self.active.push(assistant_placeholder());
    }

    /// turn 结束：把 `active` 整组并入 `bubbles`。
    pub fn finish_turn(&mut self) {
        let mut active = std::mem::take(&mut self.active);
        self.bubbles.append(&mut active);
    }

    /// 把一条服务端 `Error` 文本渲染为可见的 assistant 气泡（兜底展示）。
    pub fn render_error(&mut self, text: &str) {
        if let Some(b) = self.active.iter_mut().rfind(|b| b.is_assistant) {
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
            self.active.push(b);
        }
    }
}

// ── 流式更新 ──────────────────────────────────────

impl ChatView {
    /// 追加文本到 `active` 内最后一条 streaming 气泡。
    pub fn append_streaming_text(&mut self, chunk: &str) {
        if let Some(b) = self.active.iter_mut().rfind(|b| b.is_streaming) {
            b.text.push_str(chunk);
        }
    }

    /// 追加推理内容到 `active` 内最后一条 streaming 气泡。
    pub fn append_streaming_reasoning(&mut self, chunk: &str) {
        if let Some(b) = self.active.iter_mut().rfind(|b| b.is_streaming) {
            b.reasoning.push_str(chunk);
        }
    }

    /// 停止 streaming（`active` 内全部气泡置 `is_streaming=false`）。
    pub fn stop_streaming(&mut self) {
        for b in self.active.iter_mut() {
            b.is_streaming = false;
        }
    }

    /// 追加文本到 `active` 内最后一条 assistant 气泡。
    pub fn append_to_last_assistant(&mut self, text: &str) {
        if let Some(b) = self.active.iter_mut().rfind(|b| b.is_assistant) {
            b.text.push_str(text);
        }
    }
}

// ── Tool 调用管理 ──────────────────────────────────────

impl ChatView {
    /// `ToolCallStart`：在最后 streaming 气泡上创建 `ToolViewData`（Pending）；
    /// `is_sub_agent` 为 true 时同时初始化对应的 `AgentViewData`。
    pub fn tool_call_start(&mut self, id: &str, name: &str, is_sub_agent: bool) {
        if let Some(b) = self.active.iter_mut().rfind(|b| b.is_streaming) {
            b.tool_calls.push(ToolViewData {
                tool_call_id: id.to_string(),
                name: name.to_string(),
                arguments: String::new(),
                phase: ToolCallPhase::Pending,
                result: None,
                is_error: false,
                is_sub_agent,
            });
        }
        if is_sub_agent {
            self.agent_views.insert(
                id.to_string(),
                AgentViewData {
                    tool_call_id: id.to_string(),
                    name: name.to_string(),
                    phase: ToolCallPhase::Running,
                    events: Vec::new(),
                    is_streaming: true,
                },
            );
        }
    }

    /// `ToolCallArgsDelta`：追加参数片段。
    pub fn tool_call_append_args(&mut self, id: &str, delta: &str) {
        if let Some(b) = self.active.iter_mut().rfind(|b| b.is_streaming) {
            if let Some(tc) = b.tool_calls.iter_mut().find(|t| t.tool_call_id == id) {
                tc.arguments.push_str(delta);
            }
        }
    }

    /// `ToolCallComplete`：参数就绪。
    pub fn tool_call_complete(&mut self, id: &str, name: &str, arguments: &serde_json::Value) {
        let pretty = serde_json::to_string_pretty(arguments).unwrap_or_default();
        if let Some(b) = self.active.iter_mut().rfind(|b| b.is_streaming) {
            if let Some(tc) = b.tool_calls.iter_mut().find(|t| t.tool_call_id == id) {
                tc.arguments = pretty.clone();
                if tc.phase == ToolCallPhase::Pending {
                    tc.phase = ToolCallPhase::Running;
                }
                if tc.name.is_empty() {
                    tc.name = name.to_string();
                }
            } else {
                // 兜底：Start 事件缺失/乱序时，用事件自带 name 补建完整条目
                b.tool_calls.push(ToolViewData {
                    tool_call_id: id.to_string(),
                    name: name.to_string(),
                    arguments: pretty.clone(),
                    phase: ToolCallPhase::Running,
                    result: None,
                    is_error: false,
                    is_sub_agent: false,
                });
            }
        }
    }

    /// `ToolExecuted`：标记执行完成。
    pub fn tool_call_executed(
        &mut self,
        id: &str,
        name: &str,
        is_error: bool,
        content: &serde_json::Value,
    ) {
        let phase = if is_error {
            ToolCallPhase::Error
        } else {
            ToolCallPhase::Completed
        };
        // 遍历 active 全部气泡（可能回填到更早的、已 stop_streaming 的气泡）
        let mut found = false;
        for b in self.active.iter_mut() {
            if let Some(tc) = b.tool_calls.iter_mut().find(|t| t.tool_call_id == id) {
                tc.phase = phase.clone();
                tc.is_error = is_error;
                tc.result = Some(content.clone());
                found = true;
                break;
            }
        }
        if !found {
            // 兜底：Start/Complete 事件缺失时，在最后一个 assistant 气泡上补建条目
            let idx = self.active.iter().rposition(|b| b.is_assistant);
            if let Some(idx) = idx {
                self.active[idx].tool_calls.push(ToolViewData {
                    tool_call_id: id.to_string(),
                    name: name.to_string(),
                    arguments: String::new(),
                    phase,
                    result: Some(content.clone()),
                    is_error,
                    is_sub_agent: false,
                });
            }
        }
    }
}

// ── PendingUI / 子 agent 事件 ───────────────────────

impl ChatView {
    pub fn set_pending(&mut self, state: PendingUI) {
        self.pending_ui = Some(state);
    }

    pub fn clear_pending(&mut self) {
        self.pending_ui = None;
    }

    /// 攒入子 agent 流式事件。
    pub fn push_agent_event(&mut self, tool_call_id: &str, event: AgentEvent) {
        if let Some(av) = self.agent_views.get_mut(tool_call_id) {
            av.events.push(event);
        }
    }

    /// 子 agent 完成/出错：更新 phase，停止 streaming。
    pub fn finish_agent_view(&mut self, tool_call_id: &str, phase: ToolCallPhase) {
        if let Some(av) = self.agent_views.get_mut(tool_call_id) {
            av.phase = phase;
            av.is_streaming = false;
        }
    }

    /// 子 agent 内部发起一次工具调用（`SubChat` 里的 `ToolCallStart`）。
    ///
    /// 作为 `AgentEvent::ToolCall` 按时间顺序插入 `events` 流（与文本混排）。
    pub fn push_agent_tool_call(&mut self, agent_id: &str, inner_id: &str, name: &str) {
        if let Some(av) = self.agent_views.get_mut(agent_id) {
            av.events.push(AgentEvent::ToolCall {
                id: inner_id.to_string(),
                name: name.to_string(),
                phase: ToolCallPhase::Pending,
            });
        }
    }

    /// 就地更新子 agent 内部某次工具调用的阶段（`ToolCallComplete` → Running / `ToolExecuted` → 终态）。
    pub fn update_agent_tool_call(
        &mut self,
        agent_id: &str,
        inner_id: &str,
        phase: ToolCallPhase,
    ) {
        if let Some(av) = self.agent_views.get_mut(agent_id) {
            for ev in av.events.iter_mut() {
                if let AgentEvent::ToolCall { id, phase: p, .. } = ev {
                    if id.as_str() == inner_id {
                        *p = phase.clone();
                        break;
                    }
                }
            }
        }
    }
}

// ── 重置 ───────────────────────────────────

impl ChatView {
    /// 清空全部会话投影（保留结构，值归零）。
    pub fn clear(&mut self) {
        self.bubbles.clear();
        self.active.clear();
        self.agent_views.clear();
        self.pending_ui = None;
        self.pending_tool_call_id = None;
    }
}

// ── 状态查询 ─────────────────────────────────────────

impl ChatView {
    /// 是否正在流式输出（`active` 中任一气泡 `is_streaming=true`）。
    pub fn is_streaming(&self) -> bool {
        self.active.iter().any(|b| b.is_streaming)
    }

    /// 是否有待处理的交互卡片（`request_user_action` 挂起中）。
    pub fn has_pending(&self) -> bool {
        self.pending_ui.is_some()
    }

    /// 是否有工具调用正在执行（Pending/Running）。
    pub fn has_active_tool_call(&self) -> bool {
        self.active.iter().any(|b| {
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
