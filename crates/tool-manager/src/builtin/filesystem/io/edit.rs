//! 编辑类工具：`builtin_edit_file`。
//!
//! 语义对齐上游 `src/fs_service/io/edit.rs`（③）：
//! - 参数 `{path, edits: [{oldText, newText}], dryRun?, replaceAll?}`；
//! - 先试**精确字符串匹配**，失败则退化为**行级匹配 + 空白容忍 + 缩进保留**；
//! - 返回 git 风格 unified diff（以 ` ```diff ` 围栏包裹）；
//! - `dryRun` 为 true 时只预览、不写盘。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、**原子写**、`no_match` / `ambiguous_match`
//! 结构化错误码、`tool_audit` 审计。

use std::path::Path;
use std::time::Instant;

use serde_json::Value;
use similar::TextDiff;

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::FilesystemService;
use crate::builtin::filesystem::support::{FsError, failure, path_error, tool_result};

use super::write::atomic_write_in;

/// `builtin_edit_file`
pub(crate) async fn edit_file(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let Some(edits_value) = arguments.get("edits").and_then(Value::as_array) else {
        return failure("invalid_arguments", "edits 必须是数组");
    };
    if edits_value.is_empty() {
        return failure("invalid_arguments", "edits 至少需要一项");
    }
    let dry_run = arguments
        .get("dryRun")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let replace_all = arguments
        .get("replaceAll")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut edits: Vec<(String, String)> = Vec::with_capacity(edits_value.len());
    for (index, item) in edits_value.iter().enumerate() {
        let Some(old_text) = item.get("oldText").and_then(Value::as_str) else {
            return failure(
                "invalid_arguments",
                format!("edits[{index}].oldText 必须是字符串"),
            );
        };
        let Some(new_text) = item.get("newText").and_then(Value::as_str) else {
            return failure(
                "invalid_arguments",
                format!("edits[{index}].newText 必须是字符串"),
            );
        };
        edits.push((old_text.to_string(), new_text.to_string()));
    }

    let path = Path::new(path_str);
    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let original = match resolved.dir.read_to_string(&resolved.rel) {
        Ok(text) => text,
        Err(error) => {
            let (code, message) = path_error("读取文件", path, &error);
            return failure(code, message);
        }
    };

    let line_ending = original_line_ending(&original);
    let normalized = normalize_line_endings(&original);

    let modified = match apply_edits(&normalized, &edits, replace_all) {
        Ok(text) => text,
        Err(error) => return error.into_tool_result(),
    };

    let diff = create_unified_diff(&normalized, &modified, path_str);
    let formatted = fence_diff(&diff);

    let mut bytes_written = 0usize;
    if !dry_run {
        // 还原原文件的行尾风格后再落盘（③ 上游同款）。
        let to_write = modified.replace('\n', line_ending);
        bytes_written = to_write.len();
        if let Err(error) = atomic_write_in(&resolved, to_write.as_bytes()) {
            let (code, message) = path_error("写入文件", path, &error);
            return failure(code, message);
        }
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_edit_file",
        path = %path_str,
        edits = edits.len(),
        replace_all,
        dry_run,
        bytes_written,
        duration_ms,
        "编辑完毕"
    );

    tool_result(Value::String(formatted), false)
}

/// 依次应用编辑（照抄上游 `apply_file_edits` 的匹配与缩进规则）。
fn apply_edits(
    content: &str,
    edits: &[(String, String)],
    replace_all: bool,
) -> Result<String, FsError> {
    let mut modified = content.to_string();

    for (old_text, new_text) in edits {
        let old_norm = normalize_line_endings(old_text);
        let new_norm = normalize_line_endings(new_text);

        // ① 精确字符串匹配
        if modified.contains(&old_norm) {
            let count = modified.matches(&old_norm).count();
            if !replace_all && count > 1 {
                return Err(FsError::AmbiguousMatch(format!(
                    "oldText 在文件中出现 {count} 次；请补足上下文使其唯一，或设 replaceAll=true"
                )));
            }
            modified = if replace_all {
                modified.replace(&old_norm, &new_norm)
            } else {
                modified.replacen(&old_norm, &new_norm, 1)
            };
            continue;
        }

        // ② 行级匹配：空白容忍 + 缩进保留
        let old_lines: Vec<&str> = old_norm.trim_end().split('\n').collect();
        let mut content_lines: Vec<String> = modified
            .trim_end()
            .split('\n')
            .map(str::to_string)
            .collect();

        if old_lines.len() > content_lines.len() {
            return Err(FsError::NoMatch(format!(
                "oldText 的行数（{}）多于文件内容行数（{}），无法匹配",
                old_lines.len(),
                content_lines.len()
            )));
        }

        let max_start = content_lines.len().saturating_sub(old_lines.len());
        let mut matched_at: Option<usize> = None;
        let mut match_count = 0usize;
        for start in 0..=max_start {
            let is_match = old_lines
                .iter()
                .enumerate()
                .all(|(offset, old_line)| old_line.trim() == content_lines[start + offset].trim());
            if is_match {
                match_count += 1;
                if matched_at.is_none() {
                    matched_at = Some(start);
                }
                if !replace_all {
                    break;
                }
            }
        }

        let Some(first) = matched_at else {
            return Err(FsError::NoMatch(format!(
                "找不到与 oldText 匹配的内容：\n{old_text}"
            )));
        };
        if !replace_all && match_count > 1 {
            return Err(FsError::AmbiguousMatch(format!(
                "oldText 在文件中出现 {match_count} 次；请补足上下文使其唯一，或设 replaceAll=true"
            )));
        }

        if replace_all {
            let mut index = 0usize;
            while index + old_lines.len() <= content_lines.len() {
                let is_match = old_lines.iter().enumerate().all(|(offset, old_line)| {
                    old_line.trim() == content_lines[index + offset].trim()
                });
                if is_match {
                    let replacement =
                        rebuild_lines(&content_lines[index], &old_lines, &new_norm);
                    content_lines.splice(index..index + old_lines.len(), replacement);
                } else {
                    index += 1;
                }
            }
        } else {
            let replacement = rebuild_lines(&content_lines[first], &old_lines, &new_norm);
            content_lines.splice(first..first + old_lines.len(), replacement);
        }

        modified = content_lines.join("\n");
    }

    Ok(modified)
}

/// 用 `new_norm` 重建替换块，**按被替换块的首行缩进对齐**（照抄上游的缩进规则）。
fn rebuild_lines(anchor_line: &str, old_lines: &[&str], new_norm: &str) -> Vec<String> {
    let original_indent: String = anchor_line
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();

    new_norm
        .split('\n')
        .enumerate()
        .map(|(offset, line)| {
            if offset == 0 {
                return format!("{original_indent}{}", line.trim_start());
            }
            let old_indent: String = old_lines
                .get(offset)
                .map(|line| line.chars().take_while(|c| c.is_whitespace()).collect())
                .unwrap_or_default();
            let new_indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            let indent_char = if original_indent.contains('\t') {
                "\t"
            } else {
                " "
            };
            let relative = new_indent.len().saturating_sub(old_indent.len());
            format!(
                "{original_indent}{}{}",
                indent_char.repeat(relative),
                line.trim_start()
            )
        })
        .collect()
}

/// 生成 git 风格 unified diff（照抄上游 `create_unified_diff`）。
fn create_unified_diff(original: &str, modified: &str, file_path: &str) -> String {
    let diff = TextDiff::from_lines(original, modified);
    let patch = diff
        .unified_diff()
        .header(
            format!("{file_path}\toriginal").as_str(),
            format!("{file_path}\tmodified").as_str(),
        )
        .context_radius(4)
        .to_string();
    format!("Index: {file_path}\n{}\n{patch}", "=".repeat(68))
}

/// 用**自适应**反引号围栏包住 diff，避免正文里的 ` ``` ` 提前闭合（照抄上游）。
fn fence_diff(diff: &str) -> String {
    let mut fence = 3;
    while diff.contains(&"`".repeat(fence)) {
        fence += 1;
    }
    let backticks = "`".repeat(fence);
    format!("{backticks}diff\n{diff}{backticks}\n")
}

/// 原文件行尾风格：含 CRLF 就用 CRLF，否则 LF。
fn original_line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

/// 行尾统一为 LF（③ 上游同款：diff 与匹配都在 LF 域内做）。
fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n")
}
