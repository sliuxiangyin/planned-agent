//! 请求与事件里的文本渲染。

use serde_json::Value;

use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};

use super::SUMMARY_MAX_CHARS;

pub(crate) fn text_message(role: MessageRole, text: &str) -> Message {
    Message {
        role,
        content: Some(MessageContent::Text {
            text: text.to_string(),
        }),
        ..Default::default()
    }
}

/// 构造 tool 消息。
///
/// `MessageContent::ToolResult` 与顶层 `tool_call_id` 都要给：ai-openai 的
/// `convert_message` 要求后者非空，否则报 "Tool message must have tool_call_id"。
pub(crate) fn tool_message(tool_call_id: &str, content: &str) -> Message {
    Message {
        role: MessageRole::Tool,
        content: Some(MessageContent::ToolResult {
            tool_call_id: tool_call_id.to_string(),
            content: content.to_string(),
        }),
        tool_call_id: Some(tool_call_id.to_string()),
        ..Default::default()
    }
}

/// 取消息的文本内容。
pub(crate) fn content_text(message: &Message) -> String {
    match &message.content {
        Some(MessageContent::Text { text }) => text.clone(),
        _ => String::new(),
    }
}

/// 解析工具调用的 `arguments`。
///
/// 非法 JSON 时退回原始字符串（宁可让工具自己报错，也不丢参数）。
pub(crate) fn parse_arguments(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// 把工具入参渲染成一行日志文本（超长截断）。
///
/// 上限故意给得比输出摘要宽：入参是排查失败的现场证据，截太狠会丢掉关键那一截；
/// 但 `write_file` 之类的入参会带上整篇文件正文，完全不截断又会淹掉日志。
pub(crate) fn describe_arguments(arguments: &Value) -> String {
    const ARG_LOG_MAX_CHARS: usize = 800;
    let rendered = arguments.to_string();
    let total = rendered.chars().count();
    if total <= ARG_LOG_MAX_CHARS {
        return rendered;
    }
    let head: String = rendered.chars().take(ARG_LOG_MAX_CHARS).collect();
    format!("{head}…（已截断，共 {total} 字符）")
}

/// 把工具入参渲染成 `$` 行上的「关键参数」。
///
/// 与 [`describe_arguments`]（日志用、全量 JSON）不同，这里追求**像一条命令行**：
/// `{"command":"ls","args":["C:/x"]}` → `ls C:/x`，`{"path":"C:/x"}` → `C:/x`。
/// 太长会毁掉终端的可读性，故单行截断。
pub(crate) fn describe_tool_args(arguments: &Value) -> String {
    const ARGS_LINE_MAX_CHARS: usize = 120;

    let rendered = match arguments {
        Value::Object(map) => {
            // ① 「程序 + 参数」是执行类入参的常见形态，直接拼成命令行。
            if let Some(Value::String(command)) = map.get("command") {
                let args = match map.get("args") {
                    Some(Value::Array(items)) => {
                        items.iter().map(tool_content).collect::<Vec<_>>().join(" ")
                    }
                    Some(other) => tool_content(other),
                    None => String::new(),
                };
                format!("{command} {args}").trim_end().to_string()
            } else if map.len() == 1 {
                // ② 单字段（`{"path": ...}` / `{"query": ...}`）直接给值，读起来最像参数。
                map.values().next().map(tool_content).unwrap_or_default()
            } else {
                // ③ 其余退回紧凑 JSON：字段名本身有信息量，不该丢。
                arguments.to_string()
            }
        }
        other => tool_content(other),
    };

    if rendered.chars().count() <= ARGS_LINE_MAX_CHARS {
        return rendered;
    }
    let head: String = rendered.chars().take(ARGS_LINE_MAX_CHARS).collect();
    format!("{head}…")
}

/// 「收敛核对」轮里模型必须给出的**结论标记**（提示词与解析器共用的契约）。
///
/// 两个标记都不会互为子串（`核对未通过` 中间夹着「未」），但仍**先判未通过**再看通过：
/// 宁可把「未通过」认出来，也不要把否决误当成放行。
pub(crate) const VERIFY_PASS_MARKER: &str = "核对通过";
pub(crate) const VERIFY_FAIL_MARKER: &str = "核对未通过";

/// 读核对结论时最多扫描的前导非空行数。
///
/// 只认首行最可预测，但模型常会先写一句开场白（「好的，我来逐条对照」）再给标记；
/// 放宽到前几行是**可用性**与**可预测性**之间的折中。扫描范围刻意很小：正文（重述的产出）
/// 里的词不该被当成结论。
const VERIFY_SCAN_LINES: usize = 5;

/// 一行是不是「结论行」，是则给出结论。
///
/// 判据是**行首**（允许前导空白）而不是「行内含标记」：若本步产出正文本身会写到这些词
/// （例如产出就是一份描述本机制的文档），内含匹配会把正文误判成结论、或把正文那行删掉。
/// 代价是模型写成「好的，核对通过」这类带前缀的行会被当成**没守格式** —— 这走保守路径
///（产出退回候选产出），不会误杀。
fn verdict_of_line(line: &str) -> Option<bool> {
    let trimmed = line.trim_start();
    if trimmed.starts_with(VERIFY_FAIL_MARKER) {
        return Some(false);
    }
    if trimmed.starts_with(VERIFY_PASS_MARKER) {
        return Some(true);
    }
    None
}

/// 从核对轮的正文里读结论。
///
/// - `Some(false)`：自称未通过（模型应同时继续调工具去修）；
/// - `Some(true)`：自称通过；
/// - `None`：**没守格式** —— 调用方按「通过」处理（产出本来已经拿到，不因格式失误整步判死），
///   但**产出退回被核对的那份候选产出**（核对轮的评论正文不是产出），并留一条 warn 使这类
///   情形可观测。
pub(crate) fn verify_verdict(answer: &str) -> Option<bool> {
    let head: Vec<Option<bool>> = answer
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(VERIFY_SCAN_LINES)
        .map(verdict_of_line)
        .collect();
    // 前几行里先找「未通过」：否决优先，避免把否决误当放行。
    if head.contains(&Some(false)) {
        return Some(false);
    }
    if head.contains(&Some(true)) {
        return Some(true);
    }
    None
}

/// 去掉作为结论的那一行，返回核对轮的**正文**（即模型「原样重述」的产出）。
///
/// 只丢**第一处**行首命中：结论标记按提示词的契约只有一行。找不到标记则原样返回 ——
/// 调用方会据「是否为空」与结论有无决定回退到被核对的那一份产出。
pub(crate) fn strip_verify_marker(answer: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut dropped = false;
    for line in answer.lines() {
        if !dropped && verdict_of_line(line).is_some() {
            dropped = true;
            continue;
        }
        kept.push(line);
    }
    kept.join("\n").trim().to_string()
}

/// 工具输出转文本：字符串取原文，其余取 JSON 字面量。
pub(crate) fn tool_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// 生成输出摘要（超长截断）。
/// 按**字符**（而非字节）截断：UTF-8 安全，不 panic。
pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

pub(crate) fn summarize(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= SUMMARY_MAX_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(SUMMARY_MAX_CHARS).collect();
    format!("{head}…")
}
