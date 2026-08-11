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
use super::types::{
    display_text, ParamDef, PendingUIState, PlanSource, WorkflowPhase,
};

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
    auto_requirement: bool,
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

    // 4. 按模式分发
    tracing::info!(
        "send_message: plan_mode='{}', auto_requirement={}, text='{}'",
        plan_mode,
        auto_requirement,
        text
    );
    if plan_mode == "flexible" {
        tracing::info!("→ 走灵活模式两阶段路径");
        spawn(run_flexible_chat_stream(
            chat_svc,
            chat,
            plan_id,
            message_repo,
            auto_requirement,
        ));
    } else {
        tracing::info!("→ 走默认路径 (plan_mode 非 flexible)");
        spawn(run_chat_stream(
            chat_svc,
            chat,
            plan_id,
            message_repo,
        ));
    }
}

/// 异步消费 ChatEvent：实时写 signal 到 Dioxus runtime，流结束后持久化 assistant 消息。
async fn run_chat_stream(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut chat: ChatState,
    plan_id: String,
    message_repo: Arc<MessageRepo>,
) {
    let history: Vec<Message> = chat.messages.read().clone();
    run_chat_stream_with_history(chat_svc, chat, plan_id, message_repo, history).await;
}

/// 从指定 history 开始聊天流（供灵活模式 Phase 2 复用）。
async fn run_chat_stream_with_history(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut chat: ChatState,
    plan_id: String,
    message_repo: Arc<MessageRepo>,
    history: Vec<Message>,
) {
    let result = chat_svc
        .chat_with_callback(history, |event| match event {
            ChatEvent::TextDelta(chunk) => {
                chat.append_streaming(&chunk);
            }
            ChatEvent::ReasoningDelta(chunk) => {
                chat.append_streaming_reasoning(&chunk);
            }
            ChatEvent::UIActionRequest { message, actions } => {
                let snapshot = chat.messages.read().clone();
                chat.set_pending(PendingUIState {
                    message,
                    actions,
                    history_snapshot: snapshot,
                    trigger_phase: WorkflowPhase::Executing,
                    stage_input: None,
                });
            }
            _ => {}
        })
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
// 灵活模式：两阶段聊天（清晰度检查 → 执行）
// ─────────────────────────────────────────────────────────────────────

/// 灵活模式专用：Phase 1 清晰度检查（独立 prompt + 工具受限）→ Phase 2 完整执行。
///
/// `auto_requirement` 为 true 时，Phase 1 若 AI 追问，代码层自动选择默认动作并衔接 Phase 2，
/// 用户全程无感知；为 false 时，追问卡片展示给用户手动确认。
async fn run_flexible_chat_stream(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut chat: ChatState,
    plan_id: String,
    message_repo: Arc<MessageRepo>,
    auto_requirement: bool,
) {
    let mut history: Vec<Message> = chat.messages.read().clone();

    tracing::info!(
        "run_flexible_chat_stream: Phase 1 开始, auto_requirement={}",
        auto_requirement
    );

    // ── Phase 1：清晰度检查（独立 clarity prompt + 仅 request_user_action/builtin_read_documentation 工具） ──
    let clarity_svc = chat_svc
        .with_allowed_tools(Some(vec![
            "request_user_action".to_string(),
            "builtin_read_documentation".to_string(),
        ]))
        .with_system_prompt_template(Some("flexible/flexible_clarity_check".to_string()));

    // 自动模式：注入指令引导 AI 自行推断，避免追问
    if auto_requirement {
        tracing::info!("run_flexible_chat_stream: 注入自动模式指令");
        history.push(Message {
            role: MessageRole::System,
            content: Some(MessageContent::Text {
                text: "当前为自动执行模式。用户需求若有轻微模糊之处，请自行合理推断并继续，尽量避免使用 request_user_action 追问。仅当需求完全无法推断、执行会导致严重错误时才追问。".to_string(),
            }),
            ..Default::default()
        });
    }

    let phase1_result = match clarity_svc.chat_with_callback(history, |_| {}).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("run_flexible_chat_stream: Phase 1 失败: {}", e);
            chat.finalize_at(chat.sidx().unwrap_or(0), &format!("清晰度检查失败: {}", e));
            return;
        }
    };

    if phase1_result.cancelled {
        return;
    }

    // ── 剥离 Phase 1 的 clarity system prompt，让 Phase 2 自动注入执行 prompt ──
    let mut phase2_ready = phase1_result.history;
    if !phase2_ready.is_empty() && matches!(phase2_ready[0].role, MessageRole::System) {
        phase2_ready.remove(0);
    }

    // ── 处理 Phase 1 结果 ──
    let pending_count = phase1_result.pending_ui_actions.len();
    let phase1_text = phase2_ready
        .iter()
        .rev()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .and_then(|m| match &m.content {
            Some(MessageContent::Text { text }) => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("(无文本)");
    tracing::info!(
        "run_flexible_chat_stream: Phase 1 完成, pending_ui={}, ai_text='{}'",
        pending_count,
        phase1_text,
    );

    if !phase1_result.pending_ui_actions.is_empty() {
        tracing::info!(
            "run_flexible_chat_stream: AI 追问, auto_requirement={}",
            auto_requirement
        );
        if auto_requirement {
            // 自动模式：选默认动作，替换占位 tool result，衔接 Phase 2
            let pending = &phase1_result.pending_ui_actions[0];
            let default_choice = pending
                .actions
                .last()
                .map(|a| a.label.clone())
                .unwrap_or_else(|| "不需要，直接执行".to_string());

            for msg in phase2_ready.iter_mut().rev() {
                if matches!(msg.role, MessageRole::Tool) {
                    if let Some(MessageContent::ToolResult { ref mut content, .. }) =
                        &mut msg.content
                    {
                        if content.contains("awaiting_user_input") {
                            *content = serde_json::to_string(&serde_json::json!({
                                "choice": default_choice,
                            }))
                            .unwrap_or_else(|_| format!(r#"{{"choice":"{}"}}"#, default_choice));
                            break;
                        }
                    }
                }
            }

            run_chat_stream_with_history(chat_svc, chat, plan_id, message_repo, phase2_ready)
                .await;
        } else {
            // 手动模式：展示追问卡片给用户（snapshot 已剥离 clarity prompt）
            if let Some(last_asst) = phase2_ready
                .iter()
                .rev()
                .find(|m| matches!(m.role, MessageRole::Assistant))
            {
                if let Some(MessageContent::Text { text }) = &last_asst.content {
                    if !text.is_empty() {
                        chat.append_streaming(text);
                    }
                }
            }
            let pending = &phase1_result.pending_ui_actions[0];
            chat.set_pending(PendingUIState {
                message: pending.message.clone(),
                actions: pending.actions.clone(),
                history_snapshot: phase2_ready,
                trigger_phase: WorkflowPhase::Executing,
                stage_input: None,
            });
            chat.stop_streaming();

            // 持久化 Phase 1 追问文本
            let msgs = chat.messages.read();
            if let Some(last) = msgs.last() {
                if matches!(last.role, MessageRole::Assistant) {
                    let content = display_text(last);
                    if !content.is_empty() {
                        persist_message(&message_repo, &plan_id, "assistant", &content);
                    }
                }
            }
        }
    } else {
        // 需求明确：直接 Phase 2
        tracing::info!("run_flexible_chat_stream: 需求明确 → Phase 2 执行");
        run_chat_stream_with_history(chat_svc, chat, plan_id, message_repo, phase2_ready).await;
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

    // ── 路径 A：确认生成 → 提取计划文本 → 发出事件 → 终止 ──
    if action.id == "generate" {
        let plan_text = chat.text_at(asst_idx).unwrap_or_default();

        let source = match plan.mode().as_str() {
            "flexible" => PlanSource::Flexible,
            _ => PlanSource::Thorough,
        };

        plan.emit_generated(plan_text, source);
        chat.stop_streaming();
        chat.clear_pending();

        // 持久化当前 assistant 消息
        if let Some(ref repo) = message_repo {
            let content = chat.text_at(asst_idx).unwrap_or_default();
            if !content.is_empty() {
                persist_message(repo, &plan_id, "assistant", &content);
            }
        }

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
        let result = chat_svc
            .chat_with_callback(history, |event| match event {
                ChatEvent::TextDelta(chunk) => {
                    chat.append_streaming(&chunk);
                }
                ChatEvent::ReasoningDelta(chunk) => {
                    chat.append_streaming_reasoning(&chunk);
                }
                ChatEvent::UIActionRequest { message, actions } => {
                    let snapshot = chat.messages.read().clone();
                    chat.set_pending(PendingUIState {
                        message,
                        actions,
                        history_snapshot: snapshot,
                        trigger_phase: WorkflowPhase::Executing,
                        stage_input: None,
                    });
                }
                _ => {}
            })
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
