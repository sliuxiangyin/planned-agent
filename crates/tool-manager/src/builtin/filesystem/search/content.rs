//! `builtin_search_files_content`：在文件内容中搜索文本 / 正则。
//!
//! 语义（③）：`query` 是要找的内容；`is_regex=true` 时按正则解释；
//! `pattern`（可选）按 glob 过滤文件名；`excludePatterns` 排除；`min_bytes` / `max_bytes` 按大小过滤。
//! 返回 `路径:行号:列号: 行内容`。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、二进制跳过、单文件与结果上限、`tool_audit` 审计。
//!
//! 偏离说明：上游用 `grep` crate，本实现用 `regex` + 自写遍历（语义等价、依赖更轻）。

use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use glob_match::glob_match;
use regex::Regex;
use serde_json::Value;

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::FilesystemService;
use crate::builtin::filesystem::support::{
    failure, looks_binary, path_error, string_array, tool_result,
};

use super::{rel_to_slash, walk_all};

/// 单次返回的匹配行上限（① 防爆）。`grep_file` 的 `max_matches` 硬上限也用它。
pub(super) const MAX_MATCHES: usize = 500;
/// 单文件读取上限（① 防爆）：超过则跳过该文件。
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// 内容匹配器。
enum Matcher {
    /// 字面量（已小写化，大小写不敏感）。
    Literal(String),
    Regex(Regex),
}

impl Matcher {
    /// 返回该行所有命中的 `(起始字节, 结束字节)`。
    fn find_all(&self, line: &str) -> Vec<(usize, usize)> {
        match self {
            Matcher::Literal(needle) => {
                let lower = line.to_lowercase();
                let mut hits = Vec::new();
                let mut start = 0usize;
                while start <= lower.len() {
                    let Some(position) = lower[start..].find(needle.as_str()) else {
                        break;
                    };
                    let absolute = start + position;
                    hits.push((absolute, absolute + needle.len()));
                    start = absolute + needle.len().max(1);
                }
                hits
            }
            Matcher::Regex(regex) => regex.find_iter(line).map(|m| (m.start(), m.end())).collect(),
        }
    }
}

/// `builtin_search_files_content`
pub(crate) async fn search_files_content(
    service: &FilesystemService,
    arguments: &Value,
) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let Some(query) = arguments
        .get("query")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "query 必须是非空字符串（要搜索的内容）");
    };
    let is_regex = arguments
        .get("is_regex")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let file_pattern = arguments
        .get("pattern")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_lowercase());
    let excludes: Vec<String> = string_array(arguments.get("excludePatterns"))
        .iter()
        .map(|item| item.to_lowercase())
        .collect();
    let min_bytes = arguments.get("min_bytes").and_then(Value::as_u64);
    let max_bytes = arguments.get("max_bytes").and_then(Value::as_u64);

    let matcher = if is_regex {
        match Regex::new(query) {
            Ok(regex) => Matcher::Regex(regex),
            Err(error) => {
                return failure("regex_error", format!("正则表达式无效：{error}"));
            }
        }
    } else {
        Matcher::Literal(query.to_lowercase())
    };

    let path = Path::new(path_str);
    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let root_dir = match resolved.open_dir() {
        Ok(dir) => dir,
        Err(error) => {
            let (code, message) = path_error("打开目录", path, &error);
            return failure(code, message);
        }
    };
    let mut visited = 0usize;
    let mut walk_truncated = false;
    let items = match walk_all(&root_dir, &mut visited, &mut walk_truncated) {
        Ok(items) => items,
        Err((code, message)) => return failure(code, message),
    };

    let mut lines_out: Vec<String> = Vec::new();
    let mut truncated = false;
    let mut scanned_files = 0usize;
    let mut matched_files = 0usize;

    'files: for item in items {
        if item.is_dir {
            continue;
        }
        if item.size > MAX_FILE_BYTES {
            continue;
        }
        if let Some(min) = min_bytes {
            if item.size < min {
                continue;
            }
        }
        if let Some(max) = max_bytes {
            if item.size > max {
                continue;
            }
        }

        let rel_lower = rel_to_slash(&item.rel).to_lowercase();
        let name_lower = item
            .rel
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        if let Some(pattern) = &file_pattern {
            if !(glob_match(pattern, &name_lower) || glob_match(pattern, &rel_lower)) {
                continue;
            }
        }
        if excludes
            .iter()
            .any(|exclude| glob_match(exclude, &name_lower) || glob_match(exclude, &rel_lower))
        {
            continue;
        }

        let Ok(bytes) = root_dir.read(&item.rel) else {
            continue;
        };
        if looks_binary(&bytes) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };

        scanned_files += 1;
        let mut seen_file = false;
        for (line_index, line) in text.lines().enumerate() {
            let hits = matcher.find_all(line);
            if hits.is_empty() {
                continue;
            }
            if !seen_file {
                matched_files += 1;
                seen_file = true;
            }
            let display = resolved.display.join(&item.rel).display().to_string();
            for (start, _) in hits {
                let column = line[..start].chars().count() + 1;
                lines_out.push(format!(
                    "{display}:{}:{column}: {}",
                    line_index + 1,
                    line.trim()
                ));
                if lines_out.len() >= MAX_MATCHES {
                    truncated = true;
                    break 'files;
                }
            }
        }
    }

    let mut output = String::new();
    if lines_out.is_empty() {
        output.push_str("没有匹配的内容。\n");
    } else {
        for line in &lines_out {
            let _ = writeln!(output, "{line}");
        }
    }
    let _ = writeln!(
        output,
        "\n共 {} 处匹配（{} 个文件命中，扫描 {} 个文件）",
        lines_out.len(),
        matched_files,
        scanned_files
    );
    if walk_truncated || truncated {
        let _ = writeln!(output, "（结果被截断：到达遍历或匹配上限）");
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_search_files_content",
        path = %path_str,
        is_regex,
        matches = lines_out.len(),
        truncated = walk_truncated || truncated,
        duration_ms,
        "内容搜索完毕"
    );

    tool_result(Value::String(output), false)
}
