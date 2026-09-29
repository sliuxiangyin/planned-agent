//! 日志渲染（单行化 + 封顶）。

use super::config::LOG_OUTPUT_MAX_CHARS;
use super::super::report::ToolCallRecord; 

/// 把产出压成**单行**并封顶，供日志使用。
///
/// 两个处理都是必要的：多行会糊掉日志行（与 `system_prompt` 同一问题），
/// 不封顶则大产出会把日志淹掉。截断时会标出原始长度，便于判断是否要去读文件。
pub(super) fn log_output(text: &str) -> String {
    let mut flat = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !flat.ends_with(' ') {
                flat.push(' ');
            }
        } else {
            flat.push(ch);
        }
    }
    let flat = flat.trim();
    let total = flat.chars().count();
    if total <= LOG_OUTPUT_MAX_CHARS {
        return flat.to_string();
    }
    let head: String = flat.chars().take(LOG_OUTPUT_MAX_CHARS).collect();
    format!("{head}…（共 {total} 字符，已截断）")
}

/// 报告里工具序列的**日志摘要**：去重后的工具名，按首次出现顺序，逗号分隔。
///
/// 步骤日志里 `tool_calls=3` 只说「调了几次」，看不出「调了什么」；
/// 补上 `tools=read_file,write_file` 才能一眼判断这一步用没用（用错了）工具。
pub(super) fn summarize_tools(sequence: &[ToolCallRecord]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for call in sequence {
        if !seen.contains(&call.tool.as_str()) {
            seen.push(call.tool.as_str());
        }
    }
    seen.join(",")
}
