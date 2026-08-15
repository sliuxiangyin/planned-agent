//! Plan 页面聊天流：发送消息、异步消费 ChatEvent、用户 UI 操作回调。
//!
//! 内部按"同步准备 → spawn 异步消费"分层；所有项以 `pub(super)` 对同级 `page` 暴露。

use dioxus::prelude::*;

use crate::services::chat_service::ChatServiceSignal;
use crate::storage::repository::MessageRepo;
use planned_agent::{ChatEvent, ChatService};
use planned_agent::chat::{MultiSelectOption, UIAction, UIActionType};
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_prompt_manager::FilePromptManager;
use std::sync::Arc;

use super::states::{ChatState, PlanState};
use super::types::{display_text, ParamDef, PendingUIState};

/// 持久化一条消息到 DB（fire-and-forget）
fn persist_message(
    message_repo: &Arc<MessageRepo>,
    plan_id: &str,
    role: &str,
    content: &str,
) {
    let repo = message_repo.clone();
    let pid = plan_id.to_string();
    let r = role.to_string();
    let c = content.to_string();
    spawn(async move {
        if let Err(e) = repo.create(&pid, &r, &c).await {
            tracing::error!("持久化消息失败: {}", e);
        }
    });
}

// ─────────────────────────────────────────────────────────────────────
// 发送消息（顶层协调：同步准备 → spawn 异步消费）
// ─────────────────────────────────────────────────────────────────────

/// 同步入口：trim → push user/asst 占位 → 清输入 → 取 ChatService → spawn 异步流
pub(super) fn send_message(
    chat_signal: ChatServiceSignal,
    mut chat: ChatState,
    plan_id: String,
    message_repo: Arc<MessageRepo>,
    plan_mode: String,
) {
    let text = chat.input_text.read().trim().to_string();
    if text.is_empty() {
        return;
    }

    // 清除未响应的 UI action
    chat.clear_pending();

    // 1. 推入 User 消息 + Assistant 占位
    let _asst_idx = chat.push_user_turn(text.clone());
    chat.input_text.set(String::new());

    // 2. 持久化用户消息
    persist_message(&message_repo, &plan_id, "user", &text);

    // 3. 取 ChatService
    let chat_svc = (*chat_signal.read()).clone();
    let Some(chat_svc) = chat_svc else {
        chat.finalize_at(chat.sidx().unwrap_or(0), "AI/Tools 服务未就绪，无法发起聊天。");
        return;
    };

    // 4. plan_mode 透传给 ChatService 模板（周密模式）
    tracing::info!("send_message: plan_mode='{}', text='{}'", plan_mode, text);
    spawn(run_chat_stream(
        chat_svc,
        chat,
        plan_id,
        message_repo,
    ));
}

// ─────────────────────────────────────────────────────────────────────
// 异步消费 ChatEvent（周密模式）
// ─────────────────────────────────────────────────────────────────────

/// 异步消费 ChatEvent：实时写 signal 到 Dioxus runtime，流结束后持久化 assistant 消息。
async fn run_chat_stream(
    // TODO(重构 service.rs): ChatService 类型签名/泛型可能变化，调用方需同步。
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut chat: ChatState,
    plan_id: String,
    message_repo: Arc<MessageRepo>,
) {
    let history: Vec<Message> = chat.messages.read().clone();
    // TODO(重构 service.rs): chat_with_callback 签名/返回可能变化（详见 batch_id + HistoryStore 重构方案），调用方需同步。
    let result = chat_svc
        .chat_with_callback(history, |event| match event {
            ChatEvent::TextDelta(chunk) => {
                chat.append_streaming(&chunk);
            }
            ChatEvent::ReasoningDelta(chunk) => {
                chat.append_streaming_reasoning(&chunk);
            }
            ChatEvent::UIActionRequest {
                message,
                actions,
                ..
            } => {
                let snapshot = chat.messages.read().clone();
                chat.set_pending(PendingUIState {
                    message,
                    actions,
                    history_snapshot: snapshot,
                });
            }
            _ => {}
        }, None::<fn(planned_agent::chat::SubAgentChatEvent)>)
        .await;

    match result {
        Ok(response) if response.cancelled => {
            if let Some(idx) = chat.sidx() {
                if let Some(content) = chat.text_at(idx) {
                    if !content.is_empty() {
                        persist_message(&message_repo, &plan_id, "assistant", &content);
                    }
                }
            }
            chat.stop_streaming();
        }
        Ok(response) => {
            chat.stop_streaming();
            let msgs = chat.messages.read();
            if let Some(last) = msgs.last() {
                if matches!(last.role, MessageRole::Assistant) {
                    let content = display_text(last);
                    if !content.is_empty() {
                        let repo = message_repo.clone();
                        let pid = plan_id.clone();
                        let c = content.to_string();
                        spawn(async move {
                            let _ = repo.create(&pid, "assistant", &c).await;
                        });
                    }
                }
            }
            let _ = response;
        }
        Err(e) => {
            tracing::error!("Plan: Chat 错误: {}", e);
            chat.finalize_at(chat.sidx().unwrap_or(0), &format!("聊天失败: {}", e));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// 处理用户 UI 操作（点击按钮后的完整流程）
// ─────────────────────────────────────────────────────────────────────

pub(super) fn handle_user_action(
    action: UIAction,
    choice: String,
    pending: PendingUIState,
    mut chat: ChatState,
    chat_signal: ChatServiceSignal,
    mut plan: PlanState,
    plan_id: String,
    message_repo: Option<Arc<MessageRepo>>,
) {
    let asst_idx = chat.last_assistant_idx();
    let asst_idx = match asst_idx {
        Some(i) => i,
        None => return,
    };

    // ── 路径 A：确认生成 → 终止 ──
    if action.id == "generate" {
        chat.stop_streaming();
        chat.clear_pending();
        return;
    }

    // ── 路径 B：其他动作（如 "edit"）→ 继续对话 ──
    // 若本次动作伴随 MultiSelect 勾选（如清晰度检查的固化参数），
    // 解析为结构化 ParamDef 暂存，确认生成时随事件落库
    let params = parse_selected_params(&pending.actions, &choice);
    if !params.is_empty() {
        tracing::info!("handle_user_action: 固化参数 {} 个: {:?}", params.len(), params);
        plan.set_params(params);
    }

    chat.append_to_last_assistant(&format!("\n\n---\n\n**{}**\n\n", choice));

    chat.streaming_idx.set(Some(asst_idx));
    chat.clear_pending();

    let mut history = pending.history_snapshot;
    history.push(Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text {
            text: choice.clone(),
        }),
        ..Default::default()
    });

    let chat_svc = (*chat_signal.read()).clone();
    let Some(chat_svc) = chat_svc else {
        chat.append_text(asst_idx, "\n\n*AI 服务未就绪，无法继续对话。*");
        chat.stop_streaming();
        return;
    };

    spawn(async move {
        // TODO(重构 service.rs): chat_with_callback 签名/返回可能变化（详见 batch_id + HistoryStore 重构方案），调用方需同步。
        let result = chat_svc
            .chat_with_callback(history, |event| match event {
                ChatEvent::TextDelta(chunk) => {
                    chat.append_streaming(&chunk);
                }
                ChatEvent::ReasoningDelta(chunk) => {
                    chat.append_streaming_reasoning(&chunk);
                }
                ChatEvent::UIActionRequest {
                    message,
                    actions,
                    ..
                } => {
                    let snapshot = chat.messages.read().clone();
                    chat.set_pending(PendingUIState {
                        message,
                        actions,
                        history_snapshot: snapshot,
                    });
                }
                _ => {}
            }, None::<fn(planned_agent::chat::SubAgentChatEvent)>)
            .await;

        match result {
            Ok(response) => {
                chat.stop_streaming();

                // 持久化新的 assistant 响应
                let msgs = chat.messages.read();
                if let Some(last) = msgs.last() {
                    if matches!(last.role, MessageRole::Assistant) {
                        let content = display_text(last);
                        if !content.is_empty() {
                            if let Some(ref repo) = message_repo {
                                persist_message(repo, &plan_id, "assistant", &content);
                            }
                        }
                    }
                }

                let _ = response;
            }
            Err(e) => {
                tracing::error!("Plan: handle_user_action Chat 错误: {}", e);
                chat.append_streaming(&format!("\n\n*出错: {}*", e));
                chat.stop_streaming();
            }
        }
    });
}

/// 从 MultiSelect 类型动作中解析用户勾选项为固化参数定义。
///
/// choice 为 `id=value,id=value` 格式（见 ChatUIActionsView），
/// value 来自 option.value 字段。
/// 无 MultiSelect 动作、choice 为 "none" 或无匹配选项时返回空 Vec。
fn parse_selected_params(actions: &[UIAction], choice: &str) -> Vec<ParamDef> {
    if choice == "none" || choice.trim().is_empty() {
        return vec![];
    }

    // "param_city=北京,param_version=v2.1.0" → [("param_city", "北京"), ...]
    let selected_pairs: Vec<(&str, &str)> = choice
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            s.split_once('=').map(|(id, val)| (id.trim(), val.trim()))
        })
        .collect();

    // option id → label 查找表
    let option_map: std::collections::HashMap<&str, &MultiSelectOption> = actions
        .iter()
        .filter(|a| matches!(a.action_type, UIActionType::MultiSelect))
        .flat_map(|a| a.options.iter())
        .map(|opt| (opt.id.as_str(), opt))
        .collect();

    selected_pairs
        .iter()
        .filter_map(|(id, value_from_choice)| {
            option_map.get(id).map(|opt| {
                ParamDef {
                    name: opt.id.clone(),
                    description: opt.label.trim().to_string(),
                    example: value_from_choice.to_string(),
                }
            })
        })
        .collect()
}
