//! 加载计划数据：元数据 + 历史消息（周密模式）。
//!
//! 灵活模式由 `FlexiblePage` 内部自行维护状态，本函数只负责周密模式路径。

use std::sync::Arc;

use crate::storage::repository::{MessageRepo, PlanRepo};
use dioxus::prelude::*;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use super::super::states::{ChatState, PlanState};
use super::super::types::PlanInfo;

/// 从 DB 异步加载计划元数据 + 周密模式历史消息。
pub async fn load_plan_data(
    pid: String,
    plan_repo: Arc<PlanRepo>,
    msg_repo: Arc<MessageRepo>,
    mut chat: ChatState,
    mut plan: PlanState,
) {
    // ── 加载计划元数据 ──
    if let Ok(Some(plan_model)) = plan_repo.find_by_id(&pid).await {
        tracing::info!(
            "load_plan_data: 加载计划 '{}', mode='{}', status='{}'",
            plan_model.name,
            plan_model.mode,
            plan_model.status,
        );
        plan.plan_info.set(Some(PlanInfo {
            name: plan_model.name,
            mode: plan_model.mode.clone(),
            status: plan_model.status,
        }));
        plan.set_mode(plan_model.mode);
    }

    // ── 周密模式：加载历史消息（灵活模式由 FlexiblePage 自行管理） ──
    if plan.mode() != "flexible" {
        if let Ok(msg_list) = msg_repo.find_by_plan_id(&pid).await {
            let loaded: Vec<Message> = msg_list
                .into_iter()
                .map(|m| Message {
                    role: match m.role.as_str() {
                        "user" => MessageRole::User,
                        "assistant" => MessageRole::Assistant,
                        "system" => MessageRole::System,
                        "tool" => MessageRole::Tool,
                        _ => MessageRole::User,
                    },
                    content: if m.content.is_empty() {
                        None
                    } else {
                        Some(MessageContent::Text { text: m.content })
                    },
                    ..Default::default()
                })
                .collect();
            chat.messages.set(loaded);
            chat.reasoning_texts
                .set(vec![None; chat.messages.read().len()]);
        }
    }
}