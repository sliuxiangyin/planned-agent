//! 工具轨迹的**上层清洗**：把「真实工具调用」导成可直接落库的 `execution_trace` 产物。
//!
//! 分工刻意分成两层：
//! - 核心库 [`export_tool_trace`] 是**零策略**的 —— 历史里发生过什么就导什么，不过滤、
//!   不截断（它必须守住「轨迹 = 系统记录的事实」这条底线）；
//! - 本模块做**产品策略**：哪些调用不该进模板、单条输出留多长。
//!
//! 为什么要过滤 `request_user_action`：它是 UI 交互（高风险操作确认），不是「真实业务执行
//! 步骤」。它混进轨迹后，step5 生成模板时会把它当成一个执行步骤，产出假步骤。
//!
//! 为什么截断输出：轨迹要长期落库、还要整段喂给下游 LLM（step4/step5），单条输出动辄几十 KB
//! 会把上下文挤爆；而模板只需要「这一步大致返回了什么」，前 [`TRACE_OUTPUT_LIMIT`] 字符足够。

use planned_agent::chat::storage::StoreMessage;
use planned_agent::chat::trace::{export_tool_trace, ToolInvocation};
use serde_json::Value;

use super::super::analysis::canonicalize_windows_paths;

/// 不进入轨迹的工具名（UI 交互类）。
pub(crate) const TRACE_SKIP_TOOLS: &[&str] = &["request_user_action"];

/// 单个工具输出的保留上限（字符）。
pub(crate) const TRACE_OUTPUT_LIMIT: usize = 2000;

/// 从对话历史导出**清洗后**的工具轨迹（JSON 数组）。
///
/// 每条记录即 [`ToolInvocation`] 的序列化形态：`id / name / arguments / output / outcome`
/// （`outcome` 为 `ok` / `error` / `cancelled` / `pending`）。
///
/// 任务一个工具都没调用时返回**空数组** —— 这正是本方案的目的：让「没执行力却宣称成功」
/// 无法再被粉饰成一条漂亮轨迹（落库为空，下游一眼可见）。
pub(crate) fn export_cleaned_trace(history: &[StoreMessage]) -> Value {
    let trace: Vec<Value> = export_tool_trace(history)
        .into_iter()
        .filter(|invocation| !TRACE_SKIP_TOOLS.contains(&invocation.name.as_str()))
        .map(clean_invocation)
        .collect();
    Value::Array(trace)
}

/// 单条调用：先把 Windows 路径转正斜杠（与其它产物同一套治理，免得反斜杠在后续转拄中翻倍），
/// 再做输出长度治理；其余字段原样（入参是模板溯源的关键，绝不改写）。
fn clean_invocation(mut invocation: ToolInvocation) -> Value {
    invocation.arguments = canonicalize_windows_paths(&invocation.arguments);
    if let Some(output) = &invocation.output {
        invocation.output = Some(truncate_output(&canonicalize_windows_paths(output)));
    }
    match serde_json::to_value(&invocation) {
        Ok(value) => value,
        // `ToolInvocation` 全是可序列化数据，理论上不会失败；真失败也不静默丢记录，
        // 记 error 后落 `null`，让「这里本该有条轨迹」这件事仍然可见。
        Err(e) => {
            tracing::error!(
                "[tool_trace] 工具轨迹序列化失败（{}）：{}",
                invocation.name,
                e
            );
            Value::Null
        }
    }
}

/// 超长输出截断。
///
/// 未超限时**原样保留结构**（下游要靠它判断数据类型，如数组 / 对象）；
/// 超限时退化为「前 N 字符 + 截断提示」的字符串 —— 结构已无意义，摘要更重要。
fn truncate_output(output: &Value) -> Value {
    let text = match output {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if text.chars().count() <= TRACE_OUTPUT_LIMIT {
        return output.clone();
    }
    let head: String = text.chars().take(TRACE_OUTPUT_LIMIT).collect();
    Value::String(format!(
        "{head}…（输出过长，已截断至 {TRACE_OUTPUT_LIMIT} 字符）"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use planned_agent::chat::storage::ErrorType;
    use planned_agent_core::ai::types::{
        FunctionCall, Message, MessageContent, MessageRole, ToolCall, ToolType,
    };

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
    fn tool_result(id: &str, content: &str) -> StoreMessage {
        let msg = Message {
            role: MessageRole::Tool,
            content: Some(MessageContent::ToolResult {
                tool_call_id: id.to_string(),
                content: content.to_string(),
            }),
            tool_call_id: Some(id.to_string()),
            ..Default::default()
        };
        StoreMessage::new(msg, ErrorType::None)
    }

    #[test]
    fn empty_history_yields_empty_array() {
        // 没调用就落空数组：这正是本方案的目的（「没执行力却宣称成功」无法再被粉饰）。
        assert_eq!(export_cleaned_trace(&[]), Value::Array(vec![]));
    }

    #[test]
    fn ui_tool_is_filtered_out() {
        // request_user_action 是 UI 交互而非业务执行步骤；混进轨迹会让 step5 生成假步骤。
        let history = vec![
            assistant(&[("c1", "request_user_action", "{}")]),
            tool_result("c1", r#"{"confirmed":true}"#),
            assistant(&[("c2", "builtin_read_file", r#"{"path":"a.txt"}"#)]),
            tool_result("c2", r#""line1""#),
        ];
        let trace = export_cleaned_trace(&history);
        let arr = trace.as_array().unwrap();
        assert_eq!(arr.len(), 1, "交互工具应被过滤，只留业务工具");
        assert_eq!(arr[0]["name"], serde_json::json!("builtin_read_file"));
    }

    #[test]
    fn short_output_keeps_original_structure() {
        // 未超限时必须原样保留结构：下游要靠它判断数据类型（数组 / 对象）。
        let history = vec![
            assistant(&[("c1", "t", "{}")]),
            tool_result("c1", r#"{"a":[1,2]}"#),
        ];
        assert_eq!(
            export_cleaned_trace(&history)[0]["output"],
            serde_json::json!({ "a": [1, 2] })
        );
    }

    #[test]
    fn long_output_is_truncated_with_marker() {
        let long = "x".repeat(TRACE_OUTPUT_LIMIT + 500);
        let history = vec![
            assistant(&[("c1", "t", "{}")]),
            tool_result("c1", &serde_json::to_string(&long).unwrap()),
        ];
        let trace = export_cleaned_trace(&history);
        let out = trace[0]["output"].as_str().expect("超长输出应退化为字符串");
        assert!(out.starts_with(&"x".repeat(TRACE_OUTPUT_LIMIT)));
        assert!(out.contains("已截断"), "应带截断提示：{out}");
        assert!(out.chars().count() < long.chars().count());
    }

    #[test]
    fn windows_paths_in_arguments_become_forward_slashes() {
        // 与其它产物同一套治理：反斜杠路径落库前转正斜杠。
        let history = vec![assistant(&[(
            "c1",
            "builtin_write_file",
            r#"{"path":"C:\\Users\\woddp\\a.txt"}"#,
        )])];
        assert_eq!(
            export_cleaned_trace(&history)[0]["arguments"]["path"],
            serde_json::json!("C:/Users/woddp/a.txt")
        );
    }
}
