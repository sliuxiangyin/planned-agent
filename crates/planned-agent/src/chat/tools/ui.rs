//! `request_user_action` 工具的参数解析。
//!
//! - [`parse_ui_questions`]：从 JSON value 解析 `Vec<UIQuestion>`，逐条跳过
//!   非法问题（仅记录 warn），并确保 header 唯一（截断到 ≤4 个）。

use planned_agent_core::events::UIQuestion;
use serde_json::Value;
use tracing::warn;

/// 解析 `request_user_action` 的 questions。
///
/// - `questions` 必须是数组，逐条反序列化为 `UIQuestion`，非法条目跳过（仅 warn）；
/// - 空结果告警（前端将无内容可交互）；
/// - 超出 4 个问题时只保留前 4 个（并列问题过多会显著放大 LLM 负担）；
/// - header 重复时保留第一个（答案回传以 header 为键，必须可区分）。
pub(crate) fn parse_ui_questions(raw: &Value) -> Vec<UIQuestion> {
    let Value::Array(items) = raw else {
        warn!("request_user_action: questions 不是数组（{:?}），忽略", raw);
        return Vec::new();
    };

    let mut questions = Vec::new();
    for item in items {
        match serde_json::from_value::<UIQuestion>(item.clone()) {
            Ok(q) => {
                // 头字段必备；缺 header 时退回 question 前若干字作兜底（避免空键）
                if q.header.trim().is_empty() {
                    let fallback: String = q
                        .question
                        .chars()
                        .take(6)
                        .collect();
                    warn!("request_user_action: 问题缺 header，用 question 开头作兜底: {}", fallback);
                    questions.push(UIQuestion { header: fallback, ..q });
                } else {
                    questions.push(q);
                }
            }
            Err(e) => warn!("request_user_action: 跳过非法问题（{}）：{}", e, item),
        }
        if questions.len() >= 4 {
            warn!("request_user_action: questions 超过 4 个，只保留前 4 个");
            break;
        }
    }

    // header 去重：保留首个出现
    let mut seen = std::collections::HashSet::new();
    questions.retain(|q| seen.insert(q.header.clone()));

    if questions.is_empty() {
        warn!("request_user_action: 解析后 questions 为空，卡片将无内容可交互");
    }
    questions
}
