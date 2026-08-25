//! 聊天信号容器 —— `ChatSignals` 纯数据操作。
//!
//! 不依赖任何外部业务 crate（ChatService / ChatStorage），
//! 只操作 Signal 中的状态数据。业务流程在 `controller.rs` 中。

use dioxus::prelude::*;
use planned_agent::chat::SubscriptionGuard;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use super::types::{ChatMessage, PendingUI, ToolCallEntry, ToolCallPhase};

/// 消息状态（纯内存，Signal 均为 `Copy` 可直接进闭包/异步块）。
#[derive(Clone, Copy, PartialEq)]
pub struct ChatSignals {
    pub messages: Signal<Vec<ChatMessage>, SyncStorage>,
    pub pending_ui: Signal<Option<PendingUI>, SyncStorage>,
    pub input_text: Signal<String, SyncStorage>,
    pub pending_tool_call_id: Signal<Option<String>, SyncStorage>,
    pub subscription: Signal<Option<SubscriptionGuard>, SyncStorage>,
}

// ── 状态查询 ──────────────────────────────────────────────────────────────

impl ChatSignals {
    pub(crate) fn current_streaming(&self) -> Option<ChatMessage> {
        self.messages
            .read()
            .iter()
            .rev()
            .find(|m| m.is_streaming)
            .cloned()
    }

    pub fn is_streaming(&self) -> bool {
        self.messages.read().iter().any(|m| m.is_streaming)
    }

    pub fn last_assistant_idx(&self) -> Option<usize> {
        self.messages
            .read()
            .iter()
            .rposition(|m| matches!(m.message.role, MessageRole::Assistant))
    }

    pub fn has_pending(&self) -> bool {
        self.pending_ui.read().is_some()
    }
}

// ── 消息操作 ──────────────────────────────────────────────────────────────

impl ChatSignals {
    pub fn push_user_turn(&mut self, user_text: String, seq: &mut u64) {
        let user_msg = Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text: user_text }),
            ..Default::default()
        };
        self.messages.write().push(ChatMessage {
            message: user_msg,
            sequence_order: *seq,
            is_streaming: false,
            tool_call_id: None,
            tool_call_entries: Vec::new(),
        });
        *seq += 1;
        self.push_assistant_placeholder(seq);
    }

    pub fn push_assistant_placeholder(&mut self, seq: &mut u64) {
        let asst_msg = Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text { text: String::new() }),
            ..Default::default()
        };
        self.messages.write().push(ChatMessage {
            message: asst_msg,
            sequence_order: *seq,
            is_streaming: true,
            tool_call_id: None,
            tool_call_entries: Vec::new(),
        });
        *seq += 1;
    }

    pub fn append_text(&mut self, idx: usize, chunk: &str) {
        if let Some(cm) = self.messages.write().get_mut(idx) {
            if let Some(MessageContent::Text { text }) = &mut cm.message.content {
                text.push_str(chunk);
            }
        }
    }

    pub fn append_streaming(&mut self, chunk: &str) {
        // rfind：始终追加到最新一条 streaming 消息（多 streaming 并存时避免写错旧消息）
        if let Some(cm) = self.messages.write().iter_mut().rfind(|m| m.is_streaming) {
            if let Some(MessageContent::Text { text }) = &mut cm.message.content {
                text.push_str(chunk);
            }
        }
    }

    pub fn append_streaming_reasoning(&mut self, chunk: &str) {
        if let Some(cm) = self.messages.write().iter_mut().rfind(|m| m.is_streaming) {
            let buf = cm.message.reasoning_content.get_or_insert_with(String::new);
            buf.push_str(chunk);
        }
    }

    pub fn stop_streaming(&mut self) {
        for cm in self.messages.write().iter_mut() {
            if cm.is_streaming {
                cm.is_streaming = false;
            }
        }
    }

    pub fn append_to_last_assistant(&mut self, text: &str) {
        if let Some(idx) = self.last_assistant_idx() {
            self.append_text(idx, text);
        }
    }

    pub fn set_pending(&mut self, state: PendingUI) {
        *self.pending_ui.write() = Some(state);
    }

    pub fn clear_pending(&mut self) {
        self.pending_ui.set(None);
    }

    pub fn clear(&mut self) {
        self.messages.set(vec![]);
        self.pending_ui.set(None);
        self.pending_tool_call_id.set(None);
    }

    /// 从服务端 `ChatService::history()` 恢复展示态。
    ///
    /// 将 `Vec<Message>` 转为 `Vec<ChatMessage>`（UI 层包装），
    /// 用于页面初始化时从服务端 store 加载的历史重建。
    pub fn load_from_history(&mut self, history: &[planned_agent_core::ai::types::Message]) {
        let msgs: Vec<ChatMessage> = history
            .iter()
            .enumerate()
            .map(|(i, msg)| ChatMessage {
                message: msg.clone(),
                sequence_order: (i + 1) as u64,
                is_streaming: false,
                tool_call_id: msg.tool_call_id.clone(),
                tool_call_entries: Vec::new(), // 展示态重建由 build_bubbles 处理
            })
            .collect();
        self.messages.set(msgs);
    }

    /// 用服务端快照校准 GUI messages（`HistoryUpdated` 事件处理）。
    ///
    /// 策略：按 `sequence_order` 做差集——
    /// - 快照中不存在的消息 → 删除（服务端已回滚/清理）
    /// - 快照中有但 GUI 没有的 → 重建为非 streaming 的 ChatMessage
    /// - 正在 streaming 的消息 → 保留 GUI 侧（比快照更新；快照是回滚前截取的）
    pub fn reconcile_with_snapshot(&mut self, snapshot: &[planned_agent_core::ai::types::Message]) {
        // 收集当前 GUI 中正在 streaming 的消息
        let streaming_indices: Vec<usize> = self
            .messages
            .read()
            .iter()
            .enumerate()
            .filter(|(_, m)| m.is_streaming)
            .map(|(i, _)| i)
            .collect();
        let streaming_msgs: Vec<(usize, ChatMessage)> = {
            let msgs = self.messages.read();
            streaming_indices
                .iter()
                .map(|&i| (i, msgs[i].clone()))
                .collect()
        };

        // 快照中已有消息的 sequence_order 集合
        let snapshot_seqs: std::collections::HashSet<u64> = snapshot
            .iter()
            .enumerate()
            .map(|(i, _)| (i + 1) as u64)
            .collect();

        // 保留：快照中存在的 + 正在 streaming 的
        let mut new_msgs: Vec<ChatMessage> = Vec::with_capacity(snapshot.len());
        let mut seq = 0u64;
        for msg in snapshot {
            seq += 1;
            new_msgs.push(ChatMessage {
                message: msg.clone(),
                sequence_order: seq,
                is_streaming: false,
                tool_call_id: msg.tool_call_id.clone(),
                tool_call_entries: Vec::new(), // 历史消息无实时状态
            });
        }

        // 把正在 streaming 的消息插回（它们比快照更新，服务端不会在 streaming 时回滚）
        for (_orig_idx, sm) in &streaming_msgs {
            // 若该 streaming 消息的 sequence_order 不在快照中（被回滚了），则丢弃
            if !snapshot_seqs.contains(&sm.sequence_order) {
                continue;
            }
            // 在 new_msgs 中找到同 sequence_order 的位置，替换为 streaming 版本
            if let Some(pos) = new_msgs.iter().position(|m| m.sequence_order == sm.sequence_order) {
                new_msgs[pos] = sm.clone();
            }
        }

        self.messages.set(new_msgs);
    }
}

// ── Tool 调用管理 ─────────────────────────────────────────────────────────

impl ChatSignals {
    /// ToolCallStart：在 streaming 消息上创建 tool_call 条目。
    pub fn tool_call_start(&mut self, id: &str, name: &str) {
        if let Some(cm) = self.messages.write().iter_mut().find(|m| m.is_streaming) {
            // UI 层：tool_call_entries（实时渲染用，仅状态；name/arguments 见数据层）
            cm.tool_call_entries.push(ToolCallEntry {
                tool_call_id: id.to_string(),
                phase: ToolCallPhase::Pending,
                result: None,
                is_error: false,
            });
            // 数据层：Message.tool_calls（持久化用）
            let tool_call = planned_agent_core::ai::types::ToolCall {
                id: id.to_string(),
                r#type: planned_agent_core::ai::types::ToolType::Function,
                function: planned_agent_core::ai::types::FunctionCall {
                    name: name.to_string(),
                    arguments: String::new(),
                },
            };
            cm.message.tool_calls
                .get_or_insert_with(Vec::new)
                .push(tool_call);
        }
    }

    /// ToolCallArgsDelta：追加参数片段。
    ///
    /// 注意：服务端在同一 chunk 内可能先发 `ToolCallArgsDelta` 再发
    /// `ToolCallStart`，因此首段 delta 可能早于工具条目创建而被丢弃；
    /// 最终 `ToolCallComplete` 会用完整参数覆写（见 `tool_call_complete`），
    /// 数据不会丢失，仅流式 UI 的参数字符串可能缺首段。
    ///
    /// `name`/`arguments` 以 `Message.tool_calls` 为单一数据源，此处只更新数据层。
    pub fn tool_call_append_args(&mut self, id: &str, delta: &str) {
        if let Some(cm) = self.messages.write().iter_mut().find(|m| m.is_streaming) {
            if let Some(tcs) = &mut cm.message.tool_calls {
                if let Some(tc) = tcs.iter_mut().find(|t| t.id == id) {
                    tc.function.arguments.push_str(delta);
                }
            }
        }
    }

    /// ToolCallComplete：标记参数就绪。
    pub fn tool_call_complete(&mut self, id: &str, _name: &str, arguments: &serde_json::Value) {
        let pretty = serde_json::to_string_pretty(arguments).unwrap_or_default();
        if let Some(cm) = self.messages.write().iter_mut().find(|m| m.is_streaming) {
            // UI 层：仅更新 phase（name/arguments 以 Message.tool_calls 为权威）
            if let Some(entry) = cm
                .tool_call_entries
                .iter_mut()
                .find(|e| e.tool_call_id == id && e.phase == ToolCallPhase::Pending)
            {
                entry.phase = ToolCallPhase::Running;
            } else if !cm.tool_call_entries.iter().any(|e| e.tool_call_id == id) {
                // 兜底：Start 事件缺失/乱序（如 args delta 先行）时补建条目
                cm.tool_call_entries.push(ToolCallEntry {
                    tool_call_id: id.to_string(),
                    phase: ToolCallPhase::Running,
                    result: None,
                    is_error: false,
                });
            }
            // 数据层：按 id 覆写 Message.tool_calls 的 arguments 为完整 JSON
            if let Some(tcs) = &mut cm.message.tool_calls {
                if let Some(tc) = tcs.iter_mut().find(|t| t.id == id) {
                    tc.function.arguments = pretty;
                }
            }
        }
    }

    /// ToolExecuted：标记执行完成，并创建 Tool 消息。
    pub fn tool_call_executed(&mut self, id: &str, name: &str, is_error: bool, content: &serde_json::Value) {
        // 1. 更新 UI 层 tool_call_entries 的 phase/is_error/result（按 id 精确路由）
        let mut found = false;
        for cm in self.messages.write().iter_mut() {
            if let Some(entry) = cm.tool_call_entries.iter_mut().find(|e| e.tool_call_id == id) {
                entry.phase = if is_error { ToolCallPhase::Error } else { ToolCallPhase::Completed };
                entry.is_error = is_error;
                entry.result = Some(content.clone());
                found = true;
                break;
            }
        }
        if !found {
            // 兜底：Start/Complete 事件缺失（历史重放、request_user_action 跳过等）时
            // 在最后一个 assistant 消息上补建条目，并同步补数据层 tool_calls
            // （保证持久化后历史加载仍可见该工具调用）
            let phase = if is_error { ToolCallPhase::Error } else { ToolCallPhase::Completed };
            let target_idx = self.messages.read().iter().rposition(|m| {
                m.is_streaming || matches!(m.message.role, MessageRole::Assistant)
            });
            if let Some(idx) = target_idx {
                self.messages.write()[idx].tool_call_entries.push(ToolCallEntry {
                    tool_call_id: id.to_string(),
                    phase,
                    result: Some(content.clone()),
                    is_error,
                });
                let tool_call = planned_agent_core::ai::types::ToolCall {
                    id: id.to_string(),
                    r#type: planned_agent_core::ai::types::ToolType::Function,
                    function: planned_agent_core::ai::types::FunctionCall {
                        name: name.to_string(),
                        arguments: String::new(),
                    },
                };
                self.messages.write()[idx]
                    .message
                    .tool_calls
                    .get_or_insert_with(Vec::new)
                    .push(tool_call);
            }
        }

        // 2. 创建 Tool 消息，追加到列表末尾。
        //    正常事件序中该 assistant 即最后一条，追加 = 插在其 Tool 段尾部；
        //    乱序/迟到（下一轮已开始）时追加保证 sequence_order 与列表顺序一致
        //    （load 后按 seq 排序不错位；渲染按 tool_call_id 回填，不依赖位置）。
        let seq = self.messages.read().iter().map(|m| m.sequence_order).max().unwrap_or(0) + 1;
        let tool_content = serde_json::to_string(content).unwrap_or_default();
        let tool_msg = Message {
            role: MessageRole::Tool,
            content: Some(MessageContent::Text { text: tool_content }),
            tool_call_id: Some(id.to_string()),
            ..Default::default()
        };
        let tool_chat_msg = ChatMessage {
            message: tool_msg,
            sequence_order: seq,
            is_streaming: false,
            tool_call_id: Some(id.to_string()),
            tool_call_entries: Vec::new(),
        };
        self.messages.write().push(tool_chat_msg);
    }
}
