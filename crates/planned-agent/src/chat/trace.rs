//! 从对话历史导出**真实**的工具执行轨迹。
//!
//! 轨迹是产品的核心资产（README「Trace 是核心资产」）：它必须来自系统记录，而不是
//! 模型自述 —— 模型会漏步骤、编参数、甚至宣称调用过从未调用的工具。
//!
//! 数据源就是对话历史本身：round 层已经把每次工具调用写进了历史
//! （`push_assistant` 记 `tool_calls`、`upsert_tool` 记结果），本模块只做**导出**：
//! 不新建记录、不修改历史。因此不存在「第二份真相对不上」的问题。
//!
//! 用 [`export_tool_trace`] 拿到结构化轨迹；要区分「成功 / 执行失败 / 被取消」，
//! 必须传 [`StoreMessage`]（`Message` 会丢掉 `is_error_type`）。

use std::collections::HashMap;

use planned_agent_core::ai::types::{MessageContent, MessageRole};
use serde::Serialize;
use serde_json::Value;

use crate::chat::storage::{ErrorType, StoreMessage};

/// 一次**真实**的工具调用。
///
/// `Serialize` 供上层把轨迹直接落库（如 flexible 流程的 `execution_trace` 产物）；
/// 序列化只反映本结构，不改导出语义（仍是零策略）。
#[derive(Debug, Clone, Serialize)]
pub struct ToolInvocation {
    /// `tool_call_id` —— assistant 的声明与 tool 结果配对的键。
    pub id: String,
    /// 工具名。
    pub name: String,
    /// 真实入参（解析自 `ToolCall.function.arguments`）。
    ///
    /// 不是合法 JSON 时退化为 [`Value::String`] 保留原文，**不丢弃**。
    pub arguments: Value,
    /// 工具输出（解析自 `MessageContent::ToolResult.content`）。
    ///
    /// 同样的容错策略；未观测到结果时为 `None`。
    pub output: Option<Value>,
    /// 结局。
    pub outcome: ToolOutcome,
}

/// 工具调用的结局。
///
/// 序列化为小写下划线（`"ok"` / `"error"` / `"cancelled"` / `"pending"`），供落库后
/// 的下游（模板生成）区分「成功 / 执行失败 / 被取消 / 未跑完」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    /// 正常返回。
    Ok,
    /// 执行失败。
    Error,
    /// 被中断 / 取消。
    Cancelled,
    /// assistant 声明了调用，但历史里没有对应的 tool 结果 —— 没跑完，或流程在结果
    /// 回写之前就结束了。
    Pending,
}

impl ToolOutcome {
    fn from_error_type(et: ErrorType) -> Self {
        match et {
            ErrorType::None => Self::Ok,
            ErrorType::ExecutionError => Self::Error,
            ErrorType::Cancelled => Self::Cancelled,
        }
    }
}

/// 从对话历史导出**真实**的工具调用序列。
///
/// # 零策略
///
/// 不过滤、不截断、不改写 —— 历史里是什么就是什么。若任务一个工具都没调用，返回
/// **空 vec**：这正是本方案的目的，让「没执行力却宣称成功」无法再被粉饰成一条漂亮轨迹。
///
/// 过滤（例如剔除 UI 交互工具）与截断（例如超长输出）属于**上层**，由调用方在导出之后
/// 自行处理 —— 以免清洗规则篡改「历史里究竟发生过什么」这一事实。
///
/// # 配对规则
///
/// 以 assistant 消息里 `tool_calls` 的**原始顺序**为准（并行调用保持次序），按
/// `tool_call_id` 回填结果与结局：
///
/// - 找到对应 tool 消息 → `Ok` / `Error` / `Cancelled`（取决于 `ErrorType`）
/// - 找不到 → [`ToolOutcome::Pending`]，`output` 为 `None`
///
/// 反向不成立：**孤立**的 tool 消息（没有 assistant 声明）会被忽略 —— 保证「没被声明
/// 的调用不会凭空出现在轨迹里」。
pub fn export_tool_trace(history: &[StoreMessage]) -> Vec<ToolInvocation> {
    // 第一遍：按 tool_call_id 索引结果消息。
    let mut results: HashMap<&str, (&StoreMessage, ToolOutcome)> = HashMap::new();
    for sm in history {
        if matches!(sm.message.role, MessageRole::Tool) {
            if let Some(id) = sm.message.tool_call_id.as_deref() {
                let outcome = ToolOutcome::from_error_type(sm.is_error_type);
                // 同一个 id 只认首条：`upsert_tool` 已按 id 去重，重复即异常，
                // 此时保留先出现的（更接近「首次观测」）。
                results.entry(id).or_insert((sm, outcome));
            }
        }
    }

    // 第二遍：只从 assistant 的 tool_calls 出发，保持原始顺序。
    let mut trace = Vec::new();
    for sm in history {
        // 显式校验 role：只有 assistant 会携带 tool_calls（其余 role 写入时即为 None），
        // 目的是未来数据模型变化时不把非 assistant 的 tool_calls 也算进轨迹。
        if !matches!(sm.message.role, MessageRole::Assistant) {
            continue;
        }
        let Some(tool_calls) = &sm.message.tool_calls else {
            continue;
        };
        for tc in tool_calls {
            let matched = results.get(tc.id.as_str()).copied();
            trace.push(ToolInvocation {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
                arguments: parse_json_or_string(&tc.function.arguments),
                output: matched.and_then(|(sm, _)| tool_output(sm)),
                outcome: matched.map(|(_, o)| o).unwrap_or(ToolOutcome::Pending),
            });
        }
    }
    trace
}

/// 取 tool 消息的输出：`MessageContent::ToolResult.content` 解析为 JSON。
fn tool_output(sm: &StoreMessage) -> Option<Value> {
    match &sm.message.content {
        Some(MessageContent::ToolResult { content, .. }) => Some(parse_json_or_string(content)),
        _ => None,
    }
}

/// 解析 JSON 文本；不是 JSON 就退化为字符串保留原文（**不丢弃**）。
///
/// 两种写入路径都要活：`upsert_tool` 写的是 `serde_json::to_string` 的 JSON 文本，
/// 而 `push_cancelled_tool` 写的是裸的中断原因字符串。
fn parse_json_or_string(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use planned_agent_core::ai::types::{FunctionCall, Message, ToolCall, ToolType};

    /// 构造一条带 `tool_calls` 的 assistant 消息。入参为 `(id, name, arguments)`。
    fn assistant(calls: &[(&str, &str, &str)]) -> StoreMessage {
        let tool_calls = calls
            .iter()
            .map(|(id, name, arguments)| ToolCall {
                id: id.to_string(),
                r#type: ToolType::Function,
                function: FunctionCall {
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                },
            })
            .collect();
        let msg = Message {
            role: MessageRole::Assistant,
            tool_calls: Some(tool_calls),
            ..Default::default()
        };
        StoreMessage::new(msg, ErrorType::None)
    }

    /// 构造一条 tool 结果消息。
    fn tool_result(id: &str, content: &str, et: ErrorType) -> StoreMessage {
        let msg = Message {
            role: MessageRole::Tool,
            content: Some(MessageContent::ToolResult {
                tool_call_id: id.to_string(),
                content: content.to_string(),
            }),
            tool_call_id: Some(id.to_string()),
            ..Default::default()
        };
        StoreMessage::new(msg, et)
    }

    #[test]
    fn empty_history_yields_empty_trace() {
        // 核心保证：没调用就没轨迹，绝不凭空生成。
        assert!(export_tool_trace(&[]).is_empty());
    }

    #[test]
    fn assistant_without_tool_calls_yields_nothing() {
        let history = vec![StoreMessage::new(
            Message {
                role: MessageRole::Assistant,
                ..Default::default()
            },
            ErrorType::None,
        )];
        assert!(export_tool_trace(&history).is_empty());
    }

    #[test]
    fn pairs_call_with_result_and_keeps_order() {
        let history = vec![
            assistant(&[
                ("c1", "builtin_read_file", r#"{"path":"a.txt"}"#),
                ("c2", "builtin_write_file", r#"{"path":"b.txt"}"#),
            ]),
            tool_result("c1", r#""line1""#, ErrorType::None),
            tool_result("c2", r#""ok""#, ErrorType::None),
        ];
        let trace = export_tool_trace(&history);
        assert_eq!(trace.len(), 2);
        assert_eq!(trace[0].id, "c1");
        assert_eq!(trace[0].name, "builtin_read_file");
        assert_eq!(trace[0].arguments, serde_json::json!({ "path": "a.txt" }));
        assert_eq!(trace[0].output, Some(serde_json::json!("line1")));
        assert_eq!(trace[0].outcome, ToolOutcome::Ok);
        assert_eq!(trace[1].id, "c2");
    }

    #[test]
    fn json_arguments_are_parsed_into_object() {
        let history = vec![assistant(&[("c1", "t", r#"{"a":1}"#)])];
        assert_eq!(
            export_tool_trace(&history)[0].arguments,
            serde_json::json!({ "a": 1 })
        );
    }

    #[test]
    fn non_json_arguments_and_output_are_preserved_not_dropped() {
        let history = vec![
            assistant(&[("c1", "t", "{ not json")]),
            tool_result("c1", "被中断：用户取消", ErrorType::Cancelled),
        ];
        let trace = export_tool_trace(&history);
        assert_eq!(trace[0].arguments, Value::String("{ not json".to_string()));
        assert_eq!(
            trace[0].output,
            Some(Value::String("被中断：用户取消".to_string()))
        );
    }

    #[test]
    fn execution_error_maps_to_error() {
        let history = vec![
            assistant(&[("c1", "t", "{}")]),
            tool_result("c1", r#""boom""#, ErrorType::ExecutionError),
        ];
        assert_eq!(export_tool_trace(&history)[0].outcome, ToolOutcome::Error);
    }

    #[test]
    fn cancelled_maps_to_cancelled() {
        let history = vec![
            assistant(&[("c1", "t", "{}")]),
            tool_result("c1", "cancelled", ErrorType::Cancelled),
        ];
        assert_eq!(export_tool_trace(&history)[0].outcome, ToolOutcome::Cancelled);
    }

    #[test]
    fn declared_but_unexecuted_is_pending() {
        let history = vec![assistant(&[("c1", "t", "{}")])];
        let trace = export_tool_trace(&history);
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].outcome, ToolOutcome::Pending);
        assert!(trace[0].output.is_none());
    }

    #[test]
    fn orphan_tool_message_is_ignored() {
        // 没有 assistant 声明的 tool 消息不得凭空进入轨迹。
        let history = vec![tool_result("ghost", r#""x""#, ErrorType::None)];
        assert!(export_tool_trace(&history).is_empty());
    }

    #[test]
    fn trace_order_follows_declaration_not_result_order() {
        // 结果消息在历史里乱序时，轨迹仍按 assistant 的声明顺序输出。
        let history = vec![
            assistant(&[("c1", "first", "{}"), ("c2", "second", "{}")]),
            tool_result("c2", r#""two""#, ErrorType::None),
            tool_result("c1", r#""one""#, ErrorType::None),
        ];
        let trace = export_tool_trace(&history);
        assert_eq!(
            trace.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(trace[0].output, Some(serde_json::json!("one")));
        assert_eq!(trace[1].output, Some(serde_json::json!("two")));
    }

    #[test]
    fn multiple_assistant_rounds_accumulate_in_order() {
        let history = vec![
            assistant(&[("c1", "first", "{}")]),
            tool_result("c1", r#""1""#, ErrorType::None),
            assistant(&[("c2", "second", "{}")]),
            tool_result("c2", r#""2""#, ErrorType::None),
        ];
        let trace = export_tool_trace(&history);
        assert_eq!(
            trace.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    /// 落库契约：序列化后的字段名与 `outcome` 取值是下游（模板生成）读取的依据。
    #[test]
    fn serializes_to_stable_json_shape() {
        let history = vec![
            assistant(&[("c1", "builtin_read_file", r#"{"path":"a.txt"}"#)]),
            tool_result("c1", r#""line1""#, ErrorType::None),
            assistant(&[("c2", "builtin_write_file", r#"{"path":"b.txt"}"#)]),
        ];
        let value = serde_json::to_value(export_tool_trace(&history)).unwrap();
        assert_eq!(
            value,
            serde_json::json!([
                {
                    "id": "c1",
                    "name": "builtin_read_file",
                    "arguments": { "path": "a.txt" },
                    "output": "line1",
                    "outcome": "ok"
                },
                {
                    "id": "c2",
                    "name": "builtin_write_file",
                    "arguments": { "path": "b.txt" },
                    "output": null,
                    "outcome": "pending"
                }
            ])
        );
    }

    #[test]
    fn outcome_serializes_to_lowercase() {
        for (et, expected) in [
            (ErrorType::None, "ok"),
            (ErrorType::ExecutionError, "error"),
            (ErrorType::Cancelled, "cancelled"),
        ] {
            let history = vec![
                assistant(&[("c1", "t", "{}")]),
                tool_result("c1", r#""x""#, et),
            ];
            let value = serde_json::to_value(export_tool_trace(&history)).unwrap();
            assert_eq!(value[0]["outcome"], serde_json::json!(expected));
        }
        assert_eq!(
            serde_json::to_value(ToolOutcome::Pending).unwrap(),
            serde_json::json!("pending")
        );
    }
}
