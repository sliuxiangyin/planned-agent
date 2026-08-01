use crate::components::button::{Button, ButtonVariant};
use crate::components::markdown::Markdown;
use crate::components::resizable_panel::ResizablePanel;
use crate::components::scroll_area::ScrollArea;
use crate::components::textarea::Textarea;
use crate::components::todo::{Todo, TodoItemData, TodoStatus};
use dioxus::prelude::*;

use crate::context::{InitStatus, ModuleState};
use crate::services::chat_service::{ChatServiceSignal, use_chat_service};
use planned_agent::{ChatConfig, ChatEvent, ChatService};
use planned_agent_core::types::{Message, MessageContent, MessageRole, UIAction};
use planned_agent_prompt_manager::FilePromptManager;
use std::sync::Arc;

/// 从 `core::Message` 取出可显示文本（仅 `MessageContent::Text`；其他变体视为空串）
fn display_text(msg: &Message) -> &str {
    match &msg.content {
        Some(MessageContent::Text { text }) => text.as_str(),
        _ => "",
    }
}

/// 取可变文本引用（用于流式追加 chunk；非 `Text` 变体返回 `None`）
fn display_text_mut(msg: &mut Message) -> Option<&mut String> {
    match &mut msg.content {
        Some(MessageContent::Text { text }) => Some(text),
        _ => None,
    }
}

/// `MessageRole` → UI CSS class（仅 GUI 层使用，避免在 core 上加 UI 关注点）
fn role_css_class(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Tool => "tool",
    }
}

/// 待处理的 UI 交互状态——Agent 通过 `request_user_action` tool 请求用户操作。
#[derive(Clone)]
struct PendingUIState {
    /// 展示给用户的引导文本
    message: String,
    /// 用户可选的动作列表
    actions: Vec<UIAction>,
    /// 当时的对话历史快照（用于用户操作后继续 chat）
    history_snapshot: Vec<Message>,
}

#[component]
pub fn PlanPage() -> Element {
    // ── 全局 Context（main.rs 注入） ──
    let init_status = use_context::<Memo<InitStatus>>();

    // ── 聊天状态（SyncStorage：Send + Sync，可在 spawn 异步任务中持有） ──
    let messages = use_signal_sync(|| {
        vec![]
    });
    let mut input_text = use_signal_sync(String::new);
    // 当前正在流式输出的消息下标（None = 没有消息在流式输出）
    let streaming_idx = use_signal_sync(|| None::<usize>);
    // 待处理的 UI 交互（Agent 请求用户确认/选择/输入）
    let pending_ui = use_signal_sync(|| None::<PendingUIState>);

    // ── Chat Service 缓存（委托给 services::chat_service） ──
    // 显式传入 ChatConfig：system_prompt_template 指向 prompts/chat/system.toml
    // （当前值与 ChatConfig::default() 相同，但走显式路径便于将来按页面切模板）。
    let chat_signal = use_chat_service(ChatConfig {
        system_prompt_template: Some("chat/system".to_string()),
        ..Default::default()
    });

    // ── 按钮可用性 ──
    let can_create = init_status.read().ai.state == ModuleState::Ready
        && init_status.read().prompt.state == ModuleState::Ready;

    // 快照当前流式下标（响应式：streaming_idx 变化时本 rsx 整体重渲）
    let sidx = *streaming_idx.read();

    // ── 右侧聊天面板 ──
    let chat_panel = rsx! {
        div { class: "chat-panel",
            // Todo 计划区，固定 200px（当前使用 mock 数据，后续接 AI 计划）
            div {
                style : "padding:0px",
                Todo {
                items: vec![
                    TodoItemData::new(
                        TodoStatus::Completed,
                        "分析当前项目结构与依赖关系",
                        "读取 Cargo.toml 与 crate 目录结构，识别已实现的核心模块（core、planned-agent、agent-gui 等），确认本次改动边界。",
                    ),
                    TodoItemData::new(
                        TodoStatus::Running,
                        "创建 Todo UI 组件",
                        "在 crates/agent-gui/src/components/todo/ 下新增 mod.rs、component.rs、style.css，复用 dioxus_primitives::accordion 行为，自定义 trigger / content 样式。",
                    ),
                    TodoItemData::new(
                        TodoStatus::Queued,
                        "在 Plan 页面接入 Todo 组件",
                        "在 pages/plan.rs 的右侧聊天面板顶部插入 Todo，固定 200px 高度，列表区支持展开收起，底部执行按钮暂不接业务。",
                    ),
                    TodoItemData::new(
                        TodoStatus::Pending,
                        "后续接入 AI 计划数据流",
                        "从 Assistant 回复中解析计划条目，同步填充 Todo；执行计划时按状态机更新 TodoStatus；本轮仅完成 UI，不实现数据流。",
                    ),
                ],
                on_execute: move |_| {
                    // TODO(后续): 触发 Agent 执行计划
                },
            }
            }

            // 消息展示区
            div { class: "chat-messages",
                ScrollArea {
                    div { class: "chat-messages__list",
                        for (idx, msg) in messages.read().iter().enumerate() {
                            {
                                let is_streaming = sidx == Some(idx);
                                let text = display_text(msg);
                                let class = format!(
                                    "chat-message chat-message--{} {}",
                                    role_css_class(&msg.role),
                                    if is_streaming { "chat-message--streaming" } else { "" }
                                );
                                rsx! {
                                    div {
                                        class: "{class}",
                                        if is_streaming && text.is_empty() {
                                            "▍"
                                        } else {
                                            // Markdown 渲染：pulldown-cmark + ammonia sanitize
                                            Markdown { text: text.to_string() }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 🆕 待处理的 UI 交互（Agent 请求用户操作的按钮/选项）
            {
                let ui = pending_ui.read();
                if let Some(ref pending) = *ui {
                    rsx! {
                        div { class: "chat-ui-actions",
                            p { class: "chat-ui-actions__message", "{pending.message}" }
                            div { class: "chat-ui-actions__buttons",
                                for action in &pending.actions {
                                    {
                                        let action = action.clone();
                                        let action_for_handler = action.clone();
                                        let desc = action.description.clone().unwrap_or_default();
                                        let label = action.label.clone();
                                        let type_class = format!("chat-ui-action-btn chat-ui-action-btn--{:?}", action.action_type);
                                        let p = pending.clone();
                                        rsx! {
                                            button {
                                                class: "{type_class}",
                                                onclick: move |_| {
                                                    handle_user_action(
                                                        action_for_handler.clone(),
                                                        p.clone(),
                                                        messages,
                                                        streaming_idx,
                                                        pending_ui,
                                                        chat_signal,
                                                    );
                                                },
                                                title: "{desc}",
                                                "{label}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    rsx! {}
                }
            }

            // 输入发送区
            div { class: "chat-input-area",
                Textarea {
                    placeholder: if can_create { "创建计划..." } else { "等待 AI 与 Prompt 初始化..." },
                    value: "{input_text}",
                    disabled: !can_create,
                    oninput: move |e: FormEvent| input_text.set(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.data.key() == keyboard_types::Key::Enter && !e.data.modifiers().shift() {
                            e.prevent_default();
                            if can_create {
                                send_message(chat_signal, input_text, messages, streaming_idx, pending_ui);
                            }
                        }
                    },
                }
                Button {
                    class: "chat-input-area__submit",
                    variant: ButtonVariant::Primary,
                    disabled: !can_create,
                    title: if !can_create { Some("AI 与 Prompt 初始化完成后才能创建") } else { None },
                    onclick: move |_: MouseEvent| {
                        if can_create {
                            send_message(chat_signal, input_text, messages, streaming_idx, pending_ui);
                        }
                    },
                    "创建"
                }
            }
        }
    };

    rsx! {
        div { class: "plan-page",
            ResizablePanel {
                initial_left_percent: 70.0,
                min_left_percent: 25.0,
                max_left_percent: 75.0,
                left: rsx! {
                    div { class: "plan-left-panel",
                        span { class: "plan-left-panel__label",
                            "左侧区域（待开发）"
                        }
                    }
                },
                right: chat_panel,
            }
        }
    }
}

/// 把 Assistant 占位消息收敛为最终内容（关闭 streaming 光标）
fn finalize_assistant(
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    content: &str,
) {
    if let Some(idx) = *streaming_idx.read() {
        if let Some(msg) = messages.write().get_mut(idx) {
            if let Some(t) = display_text_mut(msg) {
                *t = content.to_string();
            }
        }
    }
    streaming_idx.set(None);
}

// ─────────────────────────────────────────────────────────────────────
// 发送消息（顶层协调：同步准备 → spawn 异步消费）
// ─────────────────────────────────────────────────────────────────────

/// 同步入口：trim → push user/asst 占位 → 清输入 → 取 ChatService → spawn 异步流
///
/// 关注点分离：
/// - 本函数：所有 *同步* 的 UI signal 副作用（在 Dioxus runtime 上下文里）
/// - `run_chat_stream`：异步消费 ChatEvent，可独立演化
fn send_message(
    chat_signal: ChatServiceSignal,
    mut input_text: Signal<String, SyncStorage>,
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    mut pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
) {
    let text = input_text.read().trim().to_string();
    if text.is_empty() {
        return;
    }

    // 清除未响应的 UI action（用户选择了直接输入文本而非点击按钮）
    pending_ui.set(None);

    // 1. 推入 User 消息 + Assistant 占位，并记录 Assistant 的下标
    let user_msg: Message = Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text { text: text.clone() }),
        ..Default::default()
    };
    let asst_msg = Message {
        role: MessageRole::Assistant,
        content: Some(MessageContent::Text { text: String::new() }),
        ..Default::default()
    };
    let asst_idx;
    {
        let mut msgs = messages.write();
        msgs.push(user_msg);
        asst_idx = msgs.len();
        msgs.push(asst_msg);
    }
    streaming_idx.set(Some(asst_idx));
    input_text.set(String::new());

    // 2. 取 ChatService（未就绪时直接 finalize 并返回）
    let chat = (*chat_signal.read()).clone();
    let Some(chat) = chat else {
        finalize_assistant(
            messages,
            streaming_idx,
            "AI/Tools 服务未就绪，无法发起聊天。",
        );
        return;
    };

    // 3. 转发到异步消费（在 spawn 里把 messages / streaming_idx / pending_ui 移交给 future）
    spawn(run_chat_stream(chat, messages, streaming_idx, pending_ui));
}

/// 异步消费 ChatEvent：实时写 signal 到 Dioxus runtime，立即 yield。
///
/// - `history` 在调用前 snapshot（一次性克隆当前 messages 列表）
/// - 消费 `TextDelta`（追加分词）和 `UIActionRequest`（设置 pending_ui）
/// - 终态收敛：有 pending_ui_actions → 关闭 streaming 光标；无 → 用 response.message 覆盖
async fn run_chat_stream(
    chat: Arc<ChatService<FilePromptManager>>,
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    mut pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
) {
    let history: Vec<Message> = messages.read().clone();
    let result = chat
        .chat_with_callback(history, |event| match event {
            ChatEvent::TextDelta(chunk) => {
                // 实时追加 chunk 到当前 streaming 的 Assistant 占位
                if let Some(idx) = *streaming_idx.read() {
                    if let Some(msg) = messages.write().get_mut(idx) {
                        if let Some(t) = display_text_mut(msg) {
                            t.push_str(&chunk);
                        }
                    }
                }
            }
            ChatEvent::UIActionRequest { message, actions } => {
                // Agent 请求用户交互：保存状态供前端渲染按钮
                *pending_ui.write() = Some(PendingUIState {
                    message,
                    actions,
                    history_snapshot: messages.read().clone(),
                });
            }
            _ => {}
        })
        .await;

    match result {
        Ok(response) => {
            if !response.pending_ui_actions.is_empty() {
                // UI actions 已通过 event 设置到 pending_ui signal
                // 清除 streaming 光标，按钮会在消息区下方渲染
                streaming_idx.set(None);
            } else {
                let final_text = display_text(&response.message).to_string();
                finalize_assistant(messages, streaming_idx, &final_text);
            }
        }
        Err(e) => {
            tracing::error!("Plan: Chat 错误: {}", e);
            finalize_assistant(messages, streaming_idx, &format!("聊天失败: {}", e));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// 处理用户 UI 操作（点击按钮后的完整流程）
// ─────────────────────────────────────────────────────────────────────

/// 用户点击 UI action 按钮后的处理：
/// 1. 替换占位 tool result 为真实用户选择
/// 2. 更新 messages → 追加 Assistant 占位
/// 3. 继续 chat_with_callback 获取 LLM 后续响应
fn handle_user_action(
    action: UIAction,
    pending: PendingUIState,
    mut messages: Signal<Vec<Message>, SyncStorage>,
    mut streaming_idx: Signal<Option<usize>, SyncStorage>,
    mut pending_ui: Signal<Option<PendingUIState>, SyncStorage>,
    chat_signal: ChatServiceSignal,
) {
    // 1. 获取历史快照
    let mut history = pending.history_snapshot;

    // 2. 替换占位 tool result 为真实的用户选择
    for msg in history.iter_mut().rev() {
        if let Some(MessageContent::ToolResult { content, .. }) = &mut msg.content {
            if content.contains("awaiting_user_input") {
                *content = serde_json::to_string(&serde_json::json!({
                    "action_id": action.id,
                    "action_type": serde_json::to_value(&action.action_type).unwrap_or_default(),
                    "choice": action.label,
                }))
                .unwrap_or_default();
                break;
            }
        }
    }

    // 3. 更新 UI messages（展示用户选择）
    messages.set(history.clone());

    // 4. 清除 pending 状态
    pending_ui.set(None);

    // 5. 添加新的 assistant 占位（用于流式输出）
    let asst_idx;
    {
        let mut msgs = messages.write();
        asst_idx = msgs.len();
        msgs.push(Message {
            role: MessageRole::Assistant,
            content: Some(MessageContent::Text {
                text: String::new(),
            }),
            ..Default::default()
        });
    }
    streaming_idx.set(Some(asst_idx));

    // 6. 继续聊天
    let chat = (*chat_signal.read()).clone();
    let Some(chat) = chat else {
        finalize_assistant(
            messages,
            streaming_idx,
            "AI 服务未就绪，无法继续对话。",
        );
        return;
    };

    spawn(async move {
        let result = chat
            .chat_with_callback(history, |event| match event {
                ChatEvent::TextDelta(chunk) => {
                    if let Some(idx) = *streaming_idx.read() {
                        if let Some(msg) = messages.write().get_mut(idx) {
                            if let Some(t) = display_text_mut(msg) {
                                t.push_str(&chunk);
                            }
                        }
                    }
                }
                ChatEvent::UIActionRequest { message, actions } => {
                    *pending_ui.write() = Some(PendingUIState {
                        message,
                        actions,
                        history_snapshot: messages.read().clone(),
                    });
                }
                _ => {}
            })
            .await;

        match result {
            Ok(response) => {
                if response.pending_ui_actions.is_empty() {
                    finalize_assistant(
                        messages,
                        streaming_idx,
                        &display_text(&response.message),
                    );
                } else {
                    streaming_idx.set(None);
                }
            }
            Err(e) => {
                tracing::error!("Plan: handle_user_action Chat 错误: {}", e);
                finalize_assistant(
                    messages,
                    streaming_idx,
                    &format!("出错: {}", e),
                );
            }
        }
    });
}

