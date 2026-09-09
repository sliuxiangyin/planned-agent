//! 纯 reducer —— 把 `ChatEvent` / 历史快照翻译为 `ChatView`。
//!
//! 与旧 `controller.rs::handle_event`（增量）+ `signals.rs::build_bubbles`（全量）
//! 两套平行逻辑不同，本模块收敛为**唯一**的翻译入口：
//!
//! - [`reduce`]：事件 → 就地更新 `ChatView`（增量路径）。
//! - [`view_from_history`]：历史快照 → 构造完整 `ChatView`（全量路径）。
//!
//! 两者共享同一套辅助函数（[`fmt_choice`] / [`REQUEST_USER_ACTION`]），
//! 消除旧代码中「request_user_action 文本化 / 子 agent 关联」两处重复实现。
//!
//! `reduce` 是纯函数（只改 `&mut ChatView`，不触碰任何 dioxus signal），
//! 可脱离 dioxus 单测。

use std::collections::HashMap;

use planned_agent::chat::storage::{ErrorType, StoreMessage};
use planned_agent::chat::ChatEvent as ServiceChatEvent;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::events::ChatEvent;
use planned_agent_core::tool_registry::types::ToolSource;

use super::types::{AgentEvent, AgentViewData, Bubble, PendingUI, ToolCallPhase, ToolViewData};
use super::view::ChatView;

/// `request_user_action` 工具名——该工具不渲染 tool_view，而是文本化进气泡。
const REQUEST_USER_ACTION: &str = "request_user_action";

/// 统一「用户选择 / request_user_action 文本」的渲染格式。
///
/// 旧代码在 `controller.rs` 与 `build_bubbles` 中各自硬编码 `\n\n---\n\n**{}**\n\n`，
/// 此处收口为唯一实现。
pub fn fmt_choice(choice: &str) -> String {
    format!("\n\n---\n\n**{}**\n\n", choice)
}

/// 消费单个 `ChatEvent`，就地更新 `ChatView`。
///
/// 与旧 `handle_event` 行为等价，差异仅一处机械变换：
/// 对 signal 的读写改为对 `ChatView` 字段的直接读写。
pub fn reduce(view: &mut ChatView, ev: &ServiceChatEvent) {
    match ev {
        ServiceChatEvent::Chat(ChatEvent::TextDelta(chunk)) => {
            view.append_streaming_text(chunk);
        }
        ServiceChatEvent::Chat(ChatEvent::ReasoningDelta(chunk)) => {
            view.append_streaming_reasoning(chunk);
        }
        ServiceChatEvent::Chat(ChatEvent::RoundStart { .. }) => {
            let was_streaming = view.is_streaming();
            tracing::info!(target: "event", event = "RoundStart", was_streaming, "RoundStart");
            if !was_streaming {
                view.push_assistant_placeholder();
            }
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name, .. })
            if name.as_str() == REQUEST_USER_ACTION =>
        {
            view.pending_tool_call_id = Some(id.clone());
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallStart { id, name, source }) => {
            let is_sub_agent = matches!(source, Some(ToolSource::SubAgent { .. }));
            tracing::info!(
                target: "event", event = "ToolCallStart",
                id = ?id, name = ?name, is_sub_agent, "ToolCallStart"
            );
            view.tool_call_start(id, name, is_sub_agent);
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallArgsDelta { id, delta }) => {
            view.tool_call_append_args(id, delta);
        }
        ServiceChatEvent::Chat(ChatEvent::ToolCallComplete {
            id,
            name,
            arguments,
        }) => {
            tracing::info!(
                target: "event", event = "ToolCallComplete",
                id = ?id, name = ?name, arguments = ?arguments, "ToolCallComplete"
            );
            if name.as_str() != REQUEST_USER_ACTION {
                view.tool_call_complete(id, name, arguments);
            }
        }
        ServiceChatEvent::Chat(ChatEvent::ToolExecuted {
            id,
            name,
            is_error,
            content,
        }) => {
            tracing::info!(
                target: "event", event = "ToolExecuted",
                id = ?id, name = ?name, is_error = *is_error, "ToolExecuted"
            );
            if name.as_str() == REQUEST_USER_ACTION {
                // request_user_action 一律文本化（与 view_from_history 的 ui_action_ids 分支一致）：
                // 中断/取消时 content 是取消原因，追加到该 request_user_action 回合气泡，不建 tool_view。
                let text = content.as_str().map(str::trim).unwrap_or("");
                if !text.is_empty() {
                    view.append_to_last_assistant(&fmt_choice(text));
                }
            } else {
                view.tool_call_executed(id, name, *is_error, content);
                // 子 agent 完成：更新 AgentView phase
                if view.agent_views.contains_key(id) {
                    let phase = if *is_error {
                        ToolCallPhase::Error
                    } else {
                        ToolCallPhase::Completed
                    };
                    view.finish_agent_view(id, phase);
                }
            }
        }
        ServiceChatEvent::Chat(ChatEvent::RoundEnd { .. }) => {
            tracing::info!(target: "event", event = "RoundEnd", "RoundEnd");
            view.stop_streaming();
        }
        ServiceChatEvent::Chat(ChatEvent::UIActionRequest {
            message,
            questions,
            session_id,
        }) => {
            let tool_call_id = view.pending_tool_call_id.clone().unwrap_or_default();
            tracing::info!(
                target: "event", event = "UIActionRequest",
                tool_call_id = ?tool_call_id, session_id = ?session_id,
                message = ?message, questions_count = questions.len(), "UIActionRequest"
            );
            view.set_pending(PendingUI {
                message: message.clone(),
                questions: questions.clone(),
                tool_call_id,
                run_id: session_id.clone(),
            });
        }
        ServiceChatEvent::Chat(ChatEvent::SubChat {
            tool_call_id,
            event,
        }) => {
            // 子 agent 流式事件：攒入对应 AgentViewData
            match event.as_ref() {
                ChatEvent::TextDelta(ref text) => {
                    view.push_agent_event(tool_call_id, AgentEvent::TextDelta(text.clone()));
                }
                ChatEvent::ReasoningDelta(ref text) => {
                    view.push_agent_event(tool_call_id, AgentEvent::ReasoningDelta(text.clone()));
                }
                _ => {}
            }
        }
        ServiceChatEvent::Done { cancelled } => {
            tracing::info!(target: "event", event = "Done", cancelled = *cancelled, "Done");
            view.stop_streaming();
            view.finish_turn();
            view.clear_pending();
            view.pending_tool_call_id = None;
        }
        ServiceChatEvent::Error(e) => {
            tracing::error!(target: "event", event = "Error", error = ?e, "聊天事件错误");
            // 兜底展示：后端可能未在不可恢复错误时补 assistant 文本，此处把错误渲染为可见文本。
            view.render_error(&format!("⚠️ 出错了：{}", e));
            view.stop_streaming();
            view.finish_turn();
            view.clear_pending();
        }
        ServiceChatEvent::HistoryUpdated { messages } => {
            tracing::info!(
                target: "event", event = "HistoryUpdated",
                count = messages.len(), "HistoryUpdated"
            );
            // 保持注释（与旧代码一致）；启用时用 `view.reconcile_with_snapshot(&messages)`。
        }
    }
}

/// 从服务端历史快照构造完整 `ChatView`（合并旧 `load_from_history` + `build_bubbles`）。
pub fn view_from_history(history: &[StoreMessage]) -> ChatView {
    // 1. 先创建 AgentViewData 骨架（从 is_agent_tool 的 assistant 消息的 tool_calls 提取 id/name）
    let mut views: HashMap<String, AgentViewData> = HashMap::new();
    for sm in history {
        if sm.is_agent_tool {
            if let Some(tcs) = &sm.message.tool_calls {
                for tc in tcs {
                    if tc.function.name != REQUEST_USER_ACTION {
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

    // 2. 重建气泡（子 agent 的 request_user_action 选择会追加到 views 里）
    let built = build_bubbles(history, Some(&mut views));

    // 3. 用 ToolViewData.result 填充 AgentViewData 的文本（最终输出）
    for bubble in &built {
        for tc in &bubble.tool_calls {
            if tc.is_sub_agent {
                if let Some(av) = views.get_mut(&tc.tool_call_id) {
                    let text = match &tc.result {
                        Some(v) => v
                            .as_str()
                            .map(String::from)
                            .or_else(|| v.get("content").and_then(|c| c.as_str()).map(String::from))
                            .unwrap_or_else(|| serde_json::to_string_pretty(v).unwrap_or_default()),
                        None => String::new(),
                    };
                    if !text.is_empty() && av.events.is_empty() {
                        av.events.push(AgentEvent::TextDelta(text));
                    }
                    av.phase = if tc.is_error {
                        ToolCallPhase::Error
                    } else {
                        ToolCallPhase::Completed
                    };
                }
            }
        }
    }

    ChatView {
        bubbles: built,
        active: Vec::new(),
        agent_views: views,
        pending_ui: None,
        pending_tool_call_id: None,
    }
}

/// 从 `StoreMessage` 序列重建气泡（纯函数）。
///
/// 消息序列规则（与旧 `signals.rs::build_bubbles` 一致）：
/// - User → 独立 user 气泡
/// - Assistant → 独立 assistant 气泡（reasoning + text + tool_calls）
/// - Tool → 正常工具按 `tool_call_id` 回填对应 `ToolViewData.result`；
///   `request_user_action` 的 Tool 消息：主 agent → 追加到 assistant 气泡文本；
///   子 agent → 追加到 `agent_views` 对应条目的 events
/// - 其他（System 等）→ 忽略
fn build_bubbles(
    messages: &[StoreMessage],
    mut agent_views: Option<&mut HashMap<String, AgentViewData>>,
) -> Vec<Bubble> {
    let mut bubbles: Vec<Bubble> = Vec::new();
    let mut tool_index: HashMap<String, (usize, usize)> = HashMap::new();
    let mut ui_action_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 追踪：当前 assistant 消息是否为子 agent，以及子 agent 在父 agent 里的 tool_call_id
    let mut last_is_agent_tool = false;
    let mut last_agent_tool_id: Option<String> = None;

    for sm in messages {
        match sm.message.role {
            MessageRole::User => {
                last_is_agent_tool = false;
                last_agent_tool_id = None;
                bubbles.push(Bubble {
                    is_assistant: false,
                    text: display_text(&sm.message).to_string(),
                    reasoning: String::new(),
                    is_streaming: false,
                    tool_calls: Vec::new(),
                });
            }
            MessageRole::Assistant => {
                last_is_agent_tool = sm.is_agent_tool;
                last_agent_tool_id = if sm.is_agent_tool {
                    sm.message.tool_calls.as_ref().and_then(|tcs| {
                        tcs.iter()
                            .find(|tc| tc.function.name != REQUEST_USER_ACTION)
                            .map(|tc| tc.id.clone())
                    })
                } else {
                    None
                };
                let tool_calls: Vec<ToolViewData> = sm
                    .message
                    .tool_calls
                    .as_ref()
                    .map(|tcs| {
                        tcs.iter()
                            .filter(|tc| {
                                if tc.function.name == REQUEST_USER_ACTION {
                                    ui_action_ids.insert(tc.id.clone());
                                    false // 不创建 ToolViewData
                                } else {
                                    true
                                }
                            })
                            .map(|tc| ToolViewData {
                                tool_call_id: tc.id.clone(),
                                name: tc.function.name.clone(),
                                arguments: tc.function.arguments.clone(),
                                phase: ToolCallPhase::Completed,
                                result: None,
                                is_error: false,
                                is_sub_agent: sm.is_agent_tool,
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
                    if ui_action_ids.contains(id) {
                        if let Some(choice) = extract_choice(&sm.message) {
                            if last_is_agent_tool {
                                // 子 agent 的 request_user_action → 追加到 agent_views
                                if let (Some(views), Some(ref agent_id)) =
                                    (agent_views.as_deref_mut(), &last_agent_tool_id)
                                {
                                    if let Some(av) = views.get_mut(agent_id) {
                                        av.events.push(AgentEvent::TextDelta(fmt_choice(&choice)));
                                    }
                                }
                            } else {
                                // 主 agent → 追加到父 assistant 气泡文本
                                if let Some(last_asst) =
                                    bubbles.iter_mut().rfind(|b| b.is_assistant)
                                {
                                    last_asst.text.push_str(&fmt_choice(&choice));
                                }
                            }
                        }
                    } else if let Some(&(b, e)) = tool_index.get(id) {
                        // 正常工具 → 回填 result
                        if let Some(result) = parse_tool_result(&sm.message) {
                            bubbles[b].tool_calls[e].result = Some(result);
                        }
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

/// 从 `request_user_action` Tool 消息的 content 中提取 `choice` 字段。
///
/// content 格式为 JSON：`{"choice":"approved","action_id":"..."}`。
fn extract_choice(msg: &Message) -> Option<String> {
    let content_text = match &msg.content {
        Some(MessageContent::ToolResult { content, .. }) => content.as_str(),
        Some(MessageContent::Text { text }) => text.as_str(),
        _ => return None,
    };
    // 尝试解析 JSON，提取 choice 字段
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(content_text) {
        if let Some(choice) = json.get("choice").and_then(|v| v.as_str()) {
            return Some(choice.to_string());
        }
    }
    // 解析失败（非 JSON）→ 原样返回（兼容纯文本场景）
    let trimmed = content_text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 解析 Tool 消息的 content（JSON 字符串）为结果值。
///
/// 同时处理两种 content 变体：
/// - `MessageContent::Text`：实时 streaming 路径创建
/// - `MessageContent::ToolResult`：服务端持久化后加载
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

#[cfg(test)]
mod tests {
    use super::{reduce, view_from_history};

    use planned_agent::chat::storage::{ErrorType, StoreMessage};
    use planned_agent::chat::ChatEvent as ServiceChatEvent;
    use planned_agent_core::ai::types::{
        FunctionCall, Message, MessageContent, MessageRole, ToolCall, ToolType,
    };
    use planned_agent_core::events::{ChatEvent, UIQuestion};
    use planned_agent_core::tool_registry::types::ToolSource;
    use serde_json::json;

    use crate::components::chat::chat_flow::types::{AgentEvent, ToolCallPhase};
    use crate::components::chat::chat_flow::view::ChatView;

    fn text_content(s: &str) -> MessageContent {
        MessageContent::Text {
            text: s.to_string(),
        }
    }

    fn question() -> UIQuestion {
        UIQuestion {
            header: "执行".to_string(),
            question: "要执行吗？".to_string(),
            options: vec![],
            multi: false,
            allow_input: false,
        }
    }

    fn round_start(view: &mut ChatView) {
        reduce(
            view,
            &ServiceChatEvent::Chat(ChatEvent::RoundStart { round: 1 }),
        );
    }

    // ── reduce：流式文本 ──────────────────────────────────────────────────

    #[test]
    fn round_start_then_text_delta_accumulates() {
        let mut view = ChatView::default();
        round_start(&mut view);
        assert_eq!(view.active.len(), 1);
        assert!(view.active[0].is_assistant);

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::TextDelta("你好".to_string())),
        );
        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::TextDelta("，世界".to_string())),
        );
        assert_eq!(view.active[0].text, "你好，世界");

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::ReasoningDelta("思考".to_string())),
        );
        assert_eq!(view.active[0].reasoning, "思考");
    }

    // ── reduce：tool 生命周期 ─────────────────────────────────────────────

    #[test]
    fn tool_call_lifecycle_creates_completed_view() {
        let mut view = ChatView::default();
        round_start(&mut view);

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::ToolCallStart {
                id: "call_1".to_string(),
                name: "tool_a".to_string(),
                source: None,
            }),
        );
        assert_eq!(view.active[0].tool_calls.len(), 1);
        assert_eq!(view.active[0].tool_calls[0].phase, ToolCallPhase::Pending);

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::ToolCallArgsDelta {
                id: "call_1".to_string(),
                delta: r#"{"a":"#.to_string(),
            }),
        );
        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::ToolCallArgsDelta {
                id: "call_1".to_string(),
                delta: r#"1}"#.to_string(),
            }),
        );
        assert_eq!(view.active[0].tool_calls[0].arguments, r#"{"a":1}"#);

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::ToolCallComplete {
                id: "call_1".to_string(),
                name: "tool_a".to_string(),
                arguments: json!({ "a": 1 }),
            }),
        );
        assert_eq!(view.active[0].tool_calls[0].phase, ToolCallPhase::Running);

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::ToolExecuted {
                id: "call_1".to_string(),
                name: "tool_a".to_string(),
                is_error: false,
                content: json!({ "result": "ok" }),
            }),
        );
        let tc = &view.active[0].tool_calls[0];
        assert_eq!(tc.phase, ToolCallPhase::Completed);
        assert_eq!(tc.is_error, false);
        assert_eq!(tc.result, Some(json!({ "result": "ok" })));
    }

    // ── reduce：request_user_action 文本化 ────────────────────────────────

    #[test]
    fn request_user_action_textualized_no_tool_view() {
        let mut view = ChatView::default();
        round_start(&mut view);

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::ToolCallStart {
                id: "ua_1".to_string(),
                name: "request_user_action".to_string(),
                source: None,
            }),
        );
        // 不建 tool_view，但记录 pending_tool_call_id
        assert!(view.active[0].tool_calls.is_empty());
        assert_eq!(view.pending_tool_call_id, Some("ua_1".to_string()));

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::ToolExecuted {
                id: "ua_1".to_string(),
                name: "request_user_action".to_string(),
                is_error: false,
                content: json!("请确认是否继续"),
            }),
        );
        // 文本化追加到 assistant 气泡
        assert!(view.active[0].tool_calls.is_empty());
        assert!(view.active[0].text.contains("请确认是否继续"));
    }

    // ── reduce：子 agent ──────────────────────────────────────────────────

    #[test]
    fn sub_agent_tool_start_and_subchat_route_to_agent_view() {
        let mut view = ChatView::default();
        round_start(&mut view);

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::ToolCallStart {
                id: "sub_1".to_string(),
                name: "flexible_step1".to_string(),
                source: Some(ToolSource::SubAgent {
                    agent_id: "sub_1".to_string(),
                }),
            }),
        );
        // 建立 ToolViewData（is_sub_agent=true）+ AgentViewData
        assert_eq!(view.active[0].tool_calls[0].is_sub_agent, true);
        assert!(view.agent_views.contains_key("sub_1"));
        assert_eq!(view.agent_views["sub_1"].phase, ToolCallPhase::Running);

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::SubChat {
                tool_call_id: "sub_1".to_string(),
                event: Box::new(ChatEvent::TextDelta("子输出".to_string())),
            }),
        );
        assert_eq!(
            view.agent_views["sub_1"].events,
            vec![AgentEvent::TextDelta("子输出".to_string())]
        );

        // ToolExecuted → 子 agent 完成，phase 置 Completed
        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::ToolExecuted {
                id: "sub_1".to_string(),
                name: "flexible_step1".to_string(),
                is_error: false,
                content: json!({ "result": "done" }),
            }),
        );
        assert_eq!(view.agent_views["sub_1"].phase, ToolCallPhase::Completed);
        assert_eq!(view.agent_views["sub_1"].is_streaming, false);
    }

    // ── reduce：UI 交互 / 生命周期 ────────────────────────────────────────

    #[test]
    fn ui_action_request_sets_pending_with_run_id() {
        let mut view = ChatView::default();
        round_start(&mut view);
        view.pending_tool_call_id = Some("ua_1".to_string());

        reduce(
            &mut view,
            &ServiceChatEvent::Chat(ChatEvent::UIActionRequest {
                message: "请选择：".to_string(),
                questions: vec![question()],
                session_id: Some("sid_9".to_string()),
            }),
        );
        let pending = view.pending_ui.clone().expect("应设置 pending");
        assert_eq!(pending.message, "请选择：");
        assert_eq!(pending.questions.len(), 1);
        assert_eq!(pending.run_id, Some("sid_9".to_string()));
        assert_eq!(pending.tool_call_id, "ua_1");
    }

    #[test]
    fn done_finishes_turn_and_clears_pending() {
        let mut view = ChatView::default();
        view.push_user_turn("你好".to_string());
        view.set_pending(crate::components::chat::chat_flow::types::PendingUI {
            message: "确认".to_string(),
            questions: vec![],
            tool_call_id: "ua_1".to_string(),
            run_id: None,
        });
        view.pending_tool_call_id = Some("ua_1".to_string());
        assert_eq!(view.active.len(), 2); // user + assistant placeholder

        reduce(&mut view, &ServiceChatEvent::Done { cancelled: false });
        assert_eq!(view.active.len(), 0);
        assert_eq!(view.bubbles.len(), 2); // active 已并入 bubbles
        assert!(view.pending_ui.is_none());
        assert_eq!(view.pending_tool_call_id, None);
    }

    #[test]
    fn error_renders_visible_text_and_finishes() {
        let mut view = ChatView::default();
        round_start(&mut view);

        reduce(&mut view, &ServiceChatEvent::Error("boom".to_string()));
        assert_eq!(view.active.len(), 0); // finish_turn 已并入
        assert!(view.bubbles.len() >= 1);
        assert!(view
            .bubbles
            .iter()
            .any(|b| b.is_assistant && b.text.contains("boom")));
    }

    // ── view_from_history：历史重建 ───────────────────────────────────────

    #[test]
    fn view_from_history_rebuilds_bubbles_and_tool_result() {
        let history = vec![
            StoreMessage::normal(Message {
                role: MessageRole::User,
                content: Some(text_content("帮我查一下")),
                ..Default::default()
            }),
            StoreMessage::normal(Message {
                role: MessageRole::Assistant,
                content: Some(text_content("好的，我来查")),
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".to_string(),
                    r#type: ToolType::Function,
                    function: FunctionCall {
                        name: "search".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                ..Default::default()
            }),
            StoreMessage::normal(Message {
                role: MessageRole::Tool,
                content: Some(MessageContent::ToolResult {
                    tool_call_id: "call_1".to_string(),
                    content: r#"{"result":"ok"}"#.to_string(),
                }),
                tool_call_id: Some("call_1".to_string()),
                ..Default::default()
            }),
        ];

        let view = view_from_history(&history);
        assert_eq!(view.bubbles.len(), 2); // user + assistant
        assert!(!view.bubbles[0].is_assistant);
        assert_eq!(view.bubbles[0].text, "帮我查一下");
        assert!(view.bubbles[1].is_assistant);
        assert_eq!(view.bubbles[1].tool_calls.len(), 1);
        assert_eq!(
            view.bubbles[1].tool_calls[0].result,
            Some(json!({ "result": "ok" }))
        );
        assert!(view.agent_views.is_empty());
    }

    #[test]
    fn view_from_history_textualizes_request_user_action_choice() {
        let history = vec![
            StoreMessage::normal(Message {
                role: MessageRole::Assistant,
                content: Some(text_content("请确认")),
                tool_calls: Some(vec![ToolCall {
                    id: "ua_1".to_string(),
                    r#type: ToolType::Function,
                    function: FunctionCall {
                        name: "request_user_action".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                ..Default::default()
            }),
            StoreMessage::normal(Message {
                role: MessageRole::Tool,
                content: Some(MessageContent::ToolResult {
                    tool_call_id: "ua_1".to_string(),
                    content: r#"{"choice":"approved"}"#.to_string(),
                }),
                tool_call_id: Some("ua_1".to_string()),
                ..Default::default()
            }),
        ];

        let view = view_from_history(&history);
        assert_eq!(view.bubbles.len(), 1);
        // request_user_action 不建 tool_view，选择被文本化
        assert!(view.bubbles[0].tool_calls.is_empty());
        assert!(view.bubbles[0].text.contains("approved"));
    }

    #[test]
    fn view_from_history_populates_sub_agent_views() {
        let history = vec![
            StoreMessage {
                message: Message {
                    role: MessageRole::Assistant,
                    content: Some(text_content("")),
                    tool_calls: Some(vec![ToolCall {
                        id: "sub_1".to_string(),
                        r#type: ToolType::Function,
                        function: FunctionCall {
                            name: "flexible_step1".to_string(),
                            arguments: "{}".to_string(),
                        },
                    }]),
                    ..Default::default()
                },
                is_error_type: ErrorType::None,
                is_agent_tool: true,
            },
            StoreMessage::normal(Message {
                role: MessageRole::Tool,
                content: Some(MessageContent::ToolResult {
                    tool_call_id: "sub_1".to_string(),
                    content: r#"{"content":"子结果"}"#.to_string(),
                }),
                tool_call_id: Some("sub_1".to_string()),
                ..Default::default()
            }),
        ];

        let view = view_from_history(&history);
        assert!(view.agent_views.contains_key("sub_1"));
        let av = &view.agent_views["sub_1"];
        assert_eq!(av.name, "flexible_step1");
        assert_eq!(av.phase, ToolCallPhase::Completed);
        assert_eq!(av.events, vec![AgentEvent::TextDelta("子结果".to_string())]);
    }
}
