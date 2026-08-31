//! Tool 调用管理 —— Start / ArgsDelta / Complete / Executed。

use dioxus::prelude::*;

use super::signals::ChatSignals;
use super::types::{AgentViewData, ToolCallPhase, ToolViewData};

impl ChatSignals {
    /// ToolCallStart：在最后 streaming 气泡上创建 `ToolViewData`（Pending）。
    ///
    /// `is_sub_agent` 为 true 时同时初始化对应的 `AgentViewData`。
    pub fn tool_call_start(&mut self, id: &str, name: &str, is_sub_agent: bool) {
        if let Some(b) = self.active.write().iter_mut().rfind(|b| b.is_streaming) {
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
            self.agent_views.write().insert(
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
                    is_sub_agent: false,
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
                    is_sub_agent: false,
                });
            }
        }
    }
}
