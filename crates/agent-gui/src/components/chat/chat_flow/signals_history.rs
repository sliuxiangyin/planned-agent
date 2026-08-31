//! 重置 / 历史加载 —— clear / load_from_history / reconcile_with_snapshot。

use std::collections::HashMap;

use dioxus::prelude::*;
use planned_agent::chat::storage::StoreMessage;

use super::signals::{build_bubbles, ChatSignals};
use super::types::{AgentEvent, AgentViewData, ToolCallPhase};

impl ChatSignals {
    pub fn clear(&mut self) {
        self.bubbles.set(vec![]);
        self.active.set(vec![]);
        self.agent_views.set(HashMap::new());
        self.pending_ui.set(None);
        self.pending_tool_call_id.set(None);
    }

    /// 从服务端 `ChatService::history_store()` 恢复气泡。
    ///
    /// 同时为 `is_sub_agent` 的工具创建 `AgentViewData`（从 ToolResult.content 恢复文本）。
    pub fn load_from_history(&mut self, history: &[StoreMessage]) {
        // 先创建 AgentViewData 骨架（从 is_agent_tool 的 assistant 消息的 tool_calls 提取 id/name）
        let mut views: HashMap<String, AgentViewData> = HashMap::new();
        for sm in history {
            if sm.is_agent_tool {
                if let Some(tcs) = &sm.message.tool_calls {
                    for tc in tcs {
                        if tc.function.name != "request_user_action" {
                            views.entry(tc.id.clone()).or_insert_with(|| AgentViewData {
                                tool_call_id: tc.id.clone(),
                                name: tc.function.name.clone(),
                                phase: ToolCallPhase::Completed,
                                events: Vec::new(),
                                is_streaming: false,
                            });
                        }
                    }
                }
            }
        }
        // 重建气泡（子 agent 的 request_user_action 选择会追加到 views 里）
        let built = build_bubbles(history, Some(&mut views));
        // 用 ToolViewData.result 填充 AgentViewData 的文本（最终输出）
        for bubble in &built {
            for tc in &bubble.tool_calls {
                if tc.is_sub_agent {
                    if let Some(av) = views.get_mut(&tc.tool_call_id) {
                        let text = match &tc.result {
                            Some(v) => {
                                v.as_str().map(String::from)
                                    .or_else(|| v.get("content").and_then(|c| c.as_str()).map(String::from))
                                    .unwrap_or_else(|| serde_json::to_string_pretty(v).unwrap_or_default())
                            }
                            None => String::new(),
                        };
                        if !text.is_empty() && av.events.is_empty() {
                            av.events.push(AgentEvent::TextDelta(text));
                        }
                        av.phase = if tc.is_error { ToolCallPhase::Error } else { ToolCallPhase::Completed };
                    }
                }
            }
        }
        self.bubbles.set(built);
        self.active.set(vec![]);
        self.agent_views.set(views);
    }

    /// 用服务端快照校准历史气泡（`HistoryUpdated` 事件处理）。
    ///
    /// 直接全量重建 `bubbles`；`active` 保留不动——正在 streaming 的气泡
    /// 物理隔离在 `active` 中，天然比快照更新、受保护，无需再按 seq 做差集。
    ///
    /// 当前 `HistoryUpdated` 分支在 controller 中保持注释，故标记允许 dead_code。
    #[allow(dead_code)]
    pub fn reconcile_with_snapshot(&mut self, snapshot: &[StoreMessage]) {
        self.bubbles.set(build_bubbles(snapshot, None));
    }
}
