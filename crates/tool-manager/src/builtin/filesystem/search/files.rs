//! `builtin_search_files`：按 glob 搜索文件名（大小写不敏感）。
//!
//! 语义对齐上游（③）：`pattern` 是 glob（如 `*.rs` / `**/*.txt`），大小写不敏感，
//! 可用 `excludePatterns` 排除、`min_bytes` / `max_bytes` 按大小过滤。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、遍历与结果上限、`tool_audit` 审计。

use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use glob_match::glob_match;
use serde_json::Value;

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::FilesystemService;
use crate::builtin::filesystem::support::{failure, path_error, string_array, tool_result};

use super::{rel_to_slash, walk_all};

/// 单次返回的匹配数上限（① 防爆）。
const MAX_RESULTS: usize = 1_000;

/// `builtin_search_files`
pub(crate) async fn search_files(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let Some(pattern) = arguments
        .get("pattern")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure(
            "invalid_arguments",
            "pattern 必须是非空字符串（glob，例如 *.rs 或 **/*.txt）",
        );
    };
    let excludes: Vec<String> = string_array(arguments.get("excludePatterns"))
        .iter()
        .map(|item| item.to_lowercase())
        .collect();
    let min_bytes = arguments.get("min_bytes").and_then(Value::as_u64);
    let max_bytes = arguments.get("max_bytes").and_then(Value::as_u64);

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

    let pattern_lower = pattern.to_lowercase();
    let mut results: Vec<String> = Vec::new();
    let mut results_truncated = false;

    for item in items {
        if item.is_dir {
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

        // 文件名与相对路径都试：`*.rs` 命中文件名，`**/*.rs` 命中路径。
        let matched =
            glob_match(&pattern_lower, &name_lower) || glob_match(&pattern_lower, &rel_lower);
        if !matched {
            continue;
        }
        if excludes
            .iter()
            .any(|exclude| glob_match(exclude, &name_lower) || glob_match(exclude, &rel_lower))
        {
            continue;
        }

        results.push(resolved.display.join(&item.rel).display().to_string());
        if results.len() >= MAX_RESULTS {
            results_truncated = true;
            break;
        }
    }

    let mut output = String::new();
    if results.is_empty() {
        output.push_str("没有匹配的文件。\n");
    } else {
        for result in &results {
            let _ = writeln!(output, "{result}");
        }
    }
    let _ = writeln!(output, "\n共 {} 个匹配", results.len());
    if walk_truncated || results_truncated {
        let _ = writeln!(output, "（结果被截断：到达遍历或结果上限）");
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_search_files",
        path = %path_str,
        pattern = %pattern,
        matches = results.len(),
        truncated = walk_truncated || results_truncated,
        duration_ms,
        "文件名搜索完毕"
    );

    tool_result(Value::String(output), false)
}
