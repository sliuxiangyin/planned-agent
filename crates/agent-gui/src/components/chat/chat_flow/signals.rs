//! 聊天信号容器 —— `ChatSignals` 纯数据操作。
//!
//! 单一气泡源：`bubbles`（历史）+ `active`（当前 turn 气泡组）。
//! 流式事件只增量更新 `active`（O(当前 turn 轮数)），`Done` 时整组并入 `bubbles`。
//!
//! 不依赖任何外部业务 crate（ChatService / ChatStorage），
//! 只操作 Signal 中的状态数据。业务流程在 `controller.rs` 中。

use std::collections::HashMap;

use dioxus::prelude::*;
use planned_agent::chat::storage::{ErrorType, StoreMessage};
use planned_agent::chat::SubscriptionGuard;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use super::types::{Bubble, PendingUI, ToolCallPhase, ToolViewData};

/// 消息状态（纯内存，Signal 均为 `Copy` 可直接进闭包/异步块）。
#[derive(Clone, Copy, PartialEq)]
pub struct ChatSignals {
    /// 历史气泡（已完成的 turn，`finish_turn` 并入）。
    pub bubbles: Signal<Vec<Bubble>, SyncStorage>,
    /// 当前 turn 气泡组（`send_message` → `Done`，流式增量更新的活跃区）。
    pub active: Signal<Vec<Bubble>, SyncStorage>,
    pub pending_ui: Signal<Option<PendingUI>, SyncStorage>,
    pub input_text: Signal<String, SyncStorage>,
    pub pending_tool_call_id: Signal<Option<String>, SyncStorage>,
    pub subscription: Signal<Option<SubscriptionGuard>, SyncStorage>,
}

// ── 状态查询 ──────────────────────────────────────────────────────────────

impl ChatSignals {
    pub fn is_streaming(&self) -> bool {
        self.active.read().iter().any(|b| b.is_streaming)
    }
    pub fn has_pending(&self) -> bool {
        self.pending_ui.read().is_some()
    }
}

// ── turn 生命周期 ─────────────────────────────────────────────────────────

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

// ── 流式更新（全部作用在 active）─────────────────────────────────────────

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

// ── PendingUI ─────────────────────────────────────────────────────────────

impl ChatSignals {
    pub fn set_pending(&mut self, state: PendingUI) {
        *self.pending_ui.write() = Some(state);
    }

    pub fn clear_pending(&mut self) {
        self.pending_ui.set(None);
    }
}

// ── 重置 / 历史 ───────────────────────────────────────────────────────────

impl ChatSignals {
    pub fn clear(&mut self) {
        self.bubbles.set(vec![]);
        self.active.set(vec![]);
        self.pending_ui.set(None);
        self.pending_tool_call_id.set(None);
    }

    /// 从服务端 `ChatService::history_store()` 恢复气泡。
    pub fn load_from_history(&mut self, history: &[StoreMessage]) {
        self.bubbles.set(build_bubbles(history));
        self.active.set(vec![]);
    }

    /// 用服务端快照校准历史气泡（`HistoryUpdated` 事件处理）。
    ///
    /// 直接全量重建 `bubbles`；`active` 保留不动——正在 streaming 的气泡
    /// 物理隔离在 `active` 中，天然比快照更新、受保护，无需再按 seq 做差集。
    ///
    /// 当前 `HistoryUpdated` 分支在 controller 中保持注释，故标记允许 dead_code。
    #[allow(dead_code)]
    pub fn reconcile_with_snapshot(&mut self, snapshot: &[StoreMessage]) {
        self.bubbles.set(build_bubbles(snapshot));
    }
}

// ── Tool 调用管理（作用在 active）────────────────────────────────────────

impl ChatSignals {
    /// ToolCallStart：在最后 streaming 气泡上创建 `ToolViewData`（Pending）。
    pub fn tool_call_start(&mut self, id: &str, name: &str) {
        if let Some(b) = self.active.write().iter_mut().rfind(|b| b.is_streaming) {
            b.tool_calls.push(ToolViewData {
                tool_call_id: id.to_string(),
                name: name.to_string(),
                arguments: String::new(),
                phase: ToolCallPhase::Pending,
                result: None,
                is_error: false,
            });
        }
    }

    /// ToolCallArgsDelta：追加参数片段。
    ///
    /// 服务端保证 `ToolCallStart` 始终先于 `ToolCallArgsDelta` 发射，
    /// 因此调用本方法时对应条目必然已创建。
    pub fn tool_call_append_args(&mut self, id: &str, delta: &str) {
        if let Some(b) = self.active.write().iter_mut().rfind(|b| b.is_streaming) {
            if let Some(tc) = b.tool_calls.iter_mut().find(|t| t.tool_call_id == id) {
                tc.arguments.push_str(delta);
            }
        }
    }

    /// ToolCallComplete：参数就绪。
    pub fn tool_call_complete(&mut self, id: &str, name: &str, arguments: &serde_json::Value) {
        let pretty = serde_json::to_string_pretty(arguments).unwrap_or_default();
        if let Some(b) = self.active.write().iter_mut().rfind(|b| b.is_streaming) {
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
                });
            }
        }
    }

    /// ToolExecuted：标记执行完成。
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
        for b in self.active.write().iter_mut() {
            if let Some(tc) = b.tool_calls.iter_mut().find(|t| t.tool_call_id == id) {
                tc.phase = phase.clone();
                tc.is_error = is_error;
                tc.result = Some(content.clone());
                found = true;
                break;
            }
        }
        if !found {
            // 兜底：Start/Complete 事件缺失（历史重放、request_user_action 跳过等）时，
            // 在最后一个 assistant 气泡上补建条目
            let idx = self.active.read().iter().rposition(|b| b.is_assistant);
            if let Some(idx) = idx {
                self.active.write()[idx].tool_calls.push(ToolViewData {
                    tool_call_id: id.to_string(),
                    name: name.to_string(),
                    arguments: String::new(),
                    phase,
                    result: Some(content.clone()),
                    is_error,
                });
            }
        }
    }
}

// ── 全量重建（历史加载 / 快照校准）───────────────────────────────────────

/// 从 `StoreMessage` 序列重建气泡（纯函数，仅用于历史加载 / 快照校准）。
///
/// 严格按 OpenAI 消息序列渲染：`User → Assistant(tool_calls) → Tool → Assistant(text)`。
/// - User → 独立 user 气泡
/// - Assistant → 独立 assistant 气泡（reasoning + text + tool_calls）
/// - Tool → 不产生气泡，按 `tool_call_id` 回填对应 `ToolViewData.result`；
///   用 `StoreMessage.is_error_type` 判断是否 Error
/// - 其他（System 等）→ 忽略
pub fn build_bubbles(messages: &[StoreMessage]) -> Vec<Bubble> {
    let mut bubbles: Vec<Bubble> = Vec::new();
    let mut tool_index: HashMap<String, (usize, usize)> = HashMap::new();

    for sm in messages {
        match sm.message.role {
            MessageRole::User => {
                bubbles.push(Bubble {
                    is_assistant: false,
                    text: display_text(&sm.message).to_string(),
                    reasoning: String::new(),
                    is_streaming: false,
                    tool_calls: Vec::new(),
                });
            }
            MessageRole::Assistant => {
                let tool_calls: Vec<ToolViewData> = sm
                    .message
                    .tool_calls
                    .as_ref()
                    .map(|tcs| {
                        tcs.iter()
                            .map(|tc| ToolViewData {
                                tool_call_id: tc.id.clone(),
                                name: tc.function.name.clone(),
                                arguments: tc.function.arguments.clone(),
                                phase: ToolCallPhase::Completed,
                                result: None,
                                is_error: false,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let bubble_idx = bubbles.len();
                for (entry_idx, tc) in tool_calls.iter().enumerate() {
                    tool_index.insert(tc.tool_call_id.clone(), (bubble_idx, entry_idx));
                }
                bubbles.push(Bubble {
                    is_assistant: true,
                    text: display_text(&sm.message).to_string(),
                    reasoning: sm.message.reasoning_content.clone().unwrap_or_default(),
                    is_streaming: false,
                    tool_calls,
                });
            }
            MessageRole::Tool => {
                if let Some(id) = sm.message.tool_call_id.as_deref() {
                    if let Some(&(b, e)) = tool_index.get(id) {
                        if let Some(result) = parse_tool_result(&sm.message) {
                            bubbles[b].tool_calls[e].result = Some(result);
                        }
                        // 根据 StoreMessage.is_error_type 判断是否出错
                        if sm.is_error_type != ErrorType::None {
                            bubbles[b].tool_calls[e].phase = ToolCallPhase::Error;
                            bubbles[b].tool_calls[e].is_error = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    bubbles
}

/// 从 `Message` 取出可显示文本。
fn display_text(msg: &Message) -> &str {
    match &msg.content {
        Some(MessageContent::Text { text }) => text.as_str(),
        _ => "",
    }
}

/// 解析 Tool 消息的 content（JSON 字符串）为结果值。
///
/// 同时处理两种 content 变体：
/// - `MessageContent::Text`：实时 streaming 路径创建（`tool_call_executed`）
/// - `MessageContent::ToolResult`：服务端持久化后加载（`push_tool`）
fn parse_tool_result(msg: &Message) -> Option<serde_json::Value> {
    let content_text = msg.content.as_ref().and_then(|c| match c {
        MessageContent::Text { text } => Some(text.as_str()),
        MessageContent::ToolResult { content, .. } => Some(content.as_str()),
        _ => None,
    })?;
    Some(
        serde_json::from_str(content_text)
            .unwrap_or_else(|_| serde_json::Value::String(content_text.to_string())),
    )
}
