//! 目录列举：`builtin_list_directory` / `builtin_list_directory_with_sizes`。
//!
//! 语义对齐上游（③）：条目带 `[FILE]` / `[DIR]` 前缀，按文件名排序；
//! `_with_sizes` 版附加人性化大小与尾部汇总。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、`tool_audit` 审计。

use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::FilesystemService;
use crate::builtin::filesystem::support::{failure, path_error, tool_result};

/// `builtin_list_directory`
pub(crate) async fn list_directory(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let path = Path::new(path_str);

    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let entries = match read_sorted_entries(&resolved) {
        Ok(entries) => entries,
        Err((code, message)) => return failure(code, message),
    };

    let mut output = String::new();
    let mut file_count = 0usize;
    let mut dir_count = 0usize;
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            dir_count += 1;
            let _ = writeln!(output, "[DIR]  {name}");
        } else {
            file_count += 1;
            let _ = writeln!(output, "[FILE] {name}");
        }
    }
    let _ = writeln!(
        output,
        "\nTotal: {file_count} files, {dir_count} directories"
    );

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_list_directory",
        path = %path_str,
        count = entries.len(),
        duration_ms,
        "目录列举完毕"
    );

    tool_result(Value::String(output), false)
}

/// `builtin_list_directory_with_sizes`
pub(crate) async fn list_directory_with_sizes(
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
    let path = Path::new(path_str);

    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let entries = match read_sorted_entries(&resolved) {
        Ok(entries) => entries,
        Err((code, message)) => return failure(code, message),
    };

    let mut output = String::new();
    let mut file_count = 0usize;
    let mut dir_count = 0usize;
    let mut total_size: u64 = 0;
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            dir_count += 1;
            let _ = writeln!(output, "[DIR]  {name:<30}");
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            file_count += 1;
            total_size += size;
            let _ = writeln!(output, "[FILE] {name:<30} {:>10}", format_bytes(size));
        }
    }
    let _ = writeln!(
        output,
        "\nTotal: {file_count} files, {dir_count} directories"
    );
    let _ = writeln!(output, "Total size: {}", format_bytes(total_size));

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_list_directory_with_sizes",
        path = %path_str,
        count = entries.len(),
        total_size,
        duration_ms,
        "目录列举完毕（带大小）"
    );

    tool_result(Value::String(output), false)
}

/// 读取目录条目并按文件名排序；错误直接落成契约错误码。
fn read_sorted_entries(
    resolved: &crate::builtin::filesystem::core::Resolved,
) -> Result<Vec<cap_std::fs::DirEntry>, (&'static str, String)> {
    let iterator = match resolved.dir.read_dir(&resolved.rel) {
        Ok(iterator) => iterator,
        Err(error) => {
            return Err(path_error("列出目录", &resolved.display, &error));
        }
    };

    let mut entries = Vec::new();
    for item in iterator {
        match item {
            Ok(entry) => entries.push(entry),
            Err(error) => {
                return Err(path_error("列出目录", &resolved.display, &error));
            }
        }
    }
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

/// 人性化字节数（上游 `format_bytes` 同款语义）。
pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}
