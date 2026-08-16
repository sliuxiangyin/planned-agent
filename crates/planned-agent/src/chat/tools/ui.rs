//! `request_user_action` 工具的参数解析。
//!
//! - [`parse_ui_actions`]：从 JSON value 解析 `Vec<UIAction>`，逐条跳过非法条目；
//! - [`parse_ui_action_lenient`]：标准反序列化失败时，若仅缺 `type` 字段则按
//!   `Confirm` 兜底重试（LLM 偶发漏掉 type）。返回 `None` 表示确实无法解析。
//!
//! 额外保护：若 `actions` 含 `MultiSelect` 但无 `Confirm`，自动在首位插入
//! 一个「确定」按钮（防交互卡死）。

use planned_agent_core::events::{
    UIAction, UIActionType, FALLBACK_CONFIRM_ID, FALLBACK_CONFIRM_LABEL,
};
use serde_json::Value;
use tracing::warn;

/// 解析 `request_user_action` 的 actions。
///
/// - 逐条跳过非法条目（仅记录 warn）；
/// - `multi_select` 无 `confirm` 时自动补「确定」按钮（防交互卡死）。
pub(crate) fn parse_ui_actions(raw: &Value) -> Vec<UIAction> {
    let mut actions = Vec::new();
    let Value::Array(items) = raw else {
        warn!("request_user_action: actions 不是数组（{:?}），忽略", raw);
        return actions;
    };
    for item in items {
        match serde_json::from_value::<UIAction>(item.clone()) {
            Ok(a) => actions.push(a),
            Err(e) => match parse_ui_action_lenient(item.clone()) {
                Some(a) => actions.push(a),
                None => warn!(
                    "request_user_action: 跳过非法 action 条目（{}）：{}",
                    e, item
                ),
            },
        }
    }
    if actions.is_empty() {
        warn!("request_user_action: 解析后 actions 为空，卡片将无可操作按钮");
    }
    // multi_select 无 confirm → 自动补「确定」按钮
    if actions
        .iter()
        .any(|a| matches!(a.action_type, UIActionType::MultiSelect))
        && !actions
            .iter()
            .any(|a| matches!(a.action_type, UIActionType::Confirm))
    {
        warn!("request_user_action: 含 multi_select 但无 confirm 按钮，自动补「确定」按钮（防交互卡死）");
        actions.insert(
            0,
            UIAction {
                id: FALLBACK_CONFIRM_ID.to_string(),
                action_type: UIActionType::Confirm,
                label: FALLBACK_CONFIRM_LABEL.to_string(),
                description: None,
                options: vec![],
            },
        );
    }
    actions
}

/// 宽松解析单个 action：标准反序列化失败时，若仅缺 `type` 字段则按
/// `Confirm` 兜底重试（LLM 偶发漏掉 type）。返回 `None` 表示确实无法解析。
fn parse_ui_action_lenient(value: Value) -> Option<UIAction> {
    if let Ok(action) = serde_json::from_value::<UIAction>(value.clone()) {
        return Some(action);
    }
    let Value::Object(mut map) = value else {
        return None;
    };
    if map.contains_key("type") {
        return None;
    }
    map.insert("type".to_string(), serde_json::json!("confirm"));
    match serde_json::from_value::<UIAction>(Value::Object(map)) {
        Ok(action) => {
            warn!("action 缺少 type 字段，已按 Confirm 兜底");
            Some(action)
        }
        Err(_) => None,
    }
}