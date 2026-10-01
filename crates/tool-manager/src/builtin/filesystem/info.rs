//! 文件元信息工具：`builtin_get_file_info`。
//!
//! 语义对齐上游（③）：返回文件/目录的详细元信息（大小、时间、权限、类型）的文本形式。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、`tool_audit` 审计。
//!
//! 本模块后续还会收纳 `calculate_directory_size` / `find_duplicate_files` /
//! `find_empty_directories`（见 `docs/planned-agent/filesystem-tools-rewrite.md` §11 阶段 4）。

use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::FilesystemService;
use crate::builtin::filesystem::support::{failure, path_error, tool_result};

/// `builtin_get_file_info`
pub(crate) async fn get_file_info(service: &FilesystemService, arguments: &Value) -> ToolResult {
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

    let metadata = match resolved.dir.metadata(&resolved.rel) {
        Ok(metadata) => metadata,
        Err(error) => {
            let (code, message) = path_error("读取元信息", path, &error);
            return failure(code, message);
        }
    };

    let info = format!(
        "path: {}\nsize: {}\ncreated: {}\nmodified: {}\naccessed: {}\nis_directory: {}\nis_file: {}\npermissions: {}",
        resolved.display.display(),
        metadata.len(),
        format_time(metadata.created().ok()),
        format_time(metadata.modified().ok()),
        format_time(metadata.accessed().ok()),
        metadata.is_dir(),
        metadata.is_file(),
        format_permissions(&metadata.permissions()),
    );

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_get_file_info",
        path = %path_str,
        is_dir = metadata.is_dir(),
        size = metadata.len(),
        duration_ms,
        "读取文件元信息完毕"
    );

    tool_result(Value::String(info), false)
}

/// 时间戳格式化（本地时区，秒级）。
///
/// 元数据来自 cap-std，其时间类型与 `std::time::SystemTime` 不同，需 `into_std()` 转换。
fn format_time(time: Option<cap_std::time::SystemTime>) -> String {
    match time.map(cap_std::time::SystemTime::into_std) {
        Some(time) => {
            let datetime: chrono::DateTime<chrono::Local> = time.into();
            datetime.format("%Y-%m-%d %H:%M:%S").to_string()
        }
        None => "unknown".to_string(),
    }
}

/// 权限的可读描述（跨平台：只区分只读 / 可写）。
fn format_permissions(permissions: &cap_std::fs::Permissions) -> String {
    if permissions.readonly() {
        "readonly".to_string()
    } else {
        "read-write".to_string()
    }
}

// ── 目录大小 / 重复文件 / 空目录（阶段 4） ──────────────────────────────────

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use glob_match::glob_match;

use crate::builtin::filesystem::search::{rel_to_slash, walk_all};
use crate::builtin::filesystem::support::string_array;

/// 参与内容比对的单文件字节上限（① 防爆）。
const MAX_COMPARE_BYTES: u64 = 64 * 1024 * 1024;

/// `builtin_calculate_directory_size`
pub(crate) async fn calculate_directory_size(
    service: &FilesystemService,
    arguments: &Value,
) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("root_path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "root_path 必须是非空字符串");
    };
    let output_format = arguments
        .get("output_format")
        .and_then(Value::as_str)
        .unwrap_or("text");

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
    let mut truncated = false;
    let items = match walk_all(&root_dir, &mut visited, &mut truncated) {
        Ok(items) => items,
        Err((code, message)) => return failure(code, message),
    };

    let mut total_size: u64 = 0;
    let mut file_count = 0usize;
    let mut dir_count = 0usize;
    for item in &items {
        if item.is_dir {
            dir_count += 1;
        } else {
            file_count += 1;
            total_size += item.size;
        }
    }

    let output = if output_format.eq_ignore_ascii_case("json") {
        serde_json::to_string_pretty(&serde_json::json!({
            "path": path_str,
            "total_size": total_size,
            "total_size_human": super::list::format_bytes(total_size),
            "file_count": file_count,
            "directory_count": dir_count,
            "truncated": truncated,
        }))
        .unwrap_or_else(|error| format!("序列化结果失败：{error}"))
    } else {
        format!(
            "path: {}\ntotal_size: {} ({})\nfile_count: {}\ndirectory_count: {}\n",
            resolved.display.display(),
            total_size,
            super::list::format_bytes(total_size),
            file_count,
            dir_count,
        )
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_calculate_directory_size",
        path = %path_str,
        total_size,
        file_count,
        duration_ms,
        "统计目录大小完毕"
    );

    tool_result(Value::String(output), false)
}

/// `builtin_find_duplicate_files`
pub(crate) async fn find_duplicate_files(
    service: &FilesystemService,
    arguments: &Value,
) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("root_path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "root_path 必须是非空字符串");
    };
    let file_pattern = arguments
        .get("pattern")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_lowercase());
    let excludes: Vec<String> = string_array(arguments.get("exclude_patterns"))
        .iter()
        .map(|item| item.to_lowercase())
        .collect();
    let min_bytes = arguments.get("min_bytes").and_then(Value::as_u64);
    let max_bytes = arguments.get("max_bytes").and_then(Value::as_u64);
    let output_format = arguments
        .get("output_format")
        .and_then(Value::as_str)
        .unwrap_or("text");

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
    let mut truncated = false;
    let items = match walk_all(&root_dir, &mut visited, &mut truncated) {
        Ok(items) => items,
        Err((code, message)) => return failure(code, message),
    };

    // 第一遍：按大小分组（大小不同的文件不可能重复）。
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for item in &items {
        if item.is_dir || item.size == 0 || item.size > MAX_COMPARE_BYTES {
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
        by_size.entry(item.size).or_default().push(item.rel.clone());
    }

    // 第二遍：同大小组内按内容分桶（零哈希依赖：直接逐字节比对）。
    let mut candidate_groups: Vec<Vec<PathBuf>> = by_size
        .into_values()
        .filter(|paths| paths.len() > 1)
        .collect();
    candidate_groups.sort();

    let mut groups: Vec<Vec<String>> = Vec::new();
    for paths in candidate_groups {
        let mut buckets: Vec<(Vec<u8>, Vec<String>)> = Vec::new();
        for rel in paths {
            let Ok(content) = root_dir.read(&rel) else {
                continue;
            };
            let display = resolved.display.join(&rel).display().to_string();
            match buckets
                .iter_mut()
                .find(|(existing, _)| *existing == content)
            {
                Some((_, group)) => group.push(display),
                None => buckets.push((content, vec![display])),
            }
        }
        for (_, group) in buckets {
            if group.len() > 1 {
                groups.push(group);
            }
        }
    }

    let output = if output_format.eq_ignore_ascii_case("json") {
        serde_json::to_string_pretty(&serde_json::json!({
            "root_path": path_str,
            "groups": groups,
            "group_count": groups.len(),
            "truncated": truncated,
        }))
        .unwrap_or_else(|error| format!("序列化结果失败：{error}"))
    } else {
        let mut text = String::new();
        if groups.is_empty() {
            text.push_str("没有重复文件。\n");
        } else {
            for (index, group) in groups.iter().enumerate() {
                let _ = writeln!(text, "组 {}（{} 个文件）:", index + 1, group.len());
                for file in group {
                    let _ = writeln!(text, "  {file}");
                }
            }
        }
        let _ = writeln!(text, "\n共 {} 组重复", groups.len());
        text
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_find_duplicate_files",
        path = %path_str,
        groups = groups.len(),
        truncated,
        duration_ms,
        "查找重复文件完毕"
    );

    tool_result(Value::String(output), false)
}

/// `builtin_find_empty_directories`
pub(crate) async fn find_empty_directories(
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
    let excludes: Vec<String> = string_array(arguments.get("exclude_patterns"))
        .iter()
        .map(|item| item.to_lowercase())
        .collect();
    let output_format = arguments
        .get("output_format")
        .and_then(Value::as_str)
        .unwrap_or("text");

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
    let mut truncated = false;
    let items = match walk_all(&root_dir, &mut visited, &mut truncated) {
        Ok(items) => items,
        Err((code, message)) => return failure(code, message),
    };

    // 被任何条目引用为父目录的目录集合 = 非空目录集合。
    let mut non_empty: HashSet<PathBuf> = HashSet::new();
    for item in &items {
        let parent = match item.rel.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        non_empty.insert(parent);
    }

    let mut empties: Vec<String> = Vec::new();
    for item in &items {
        if !item.is_dir {
            continue;
        }
        let rel_lower = rel_to_slash(&item.rel).to_lowercase();
        let name_lower = item
            .rel
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if excludes
            .iter()
            .any(|exclude| glob_match(exclude, &name_lower) || glob_match(exclude, &rel_lower))
        {
            continue;
        }
        if !non_empty.contains(&item.rel) {
            empties.push(resolved.display.join(&item.rel).display().to_string());
        }
    }

    let output = if output_format.eq_ignore_ascii_case("json") {
        serde_json::to_string_pretty(&serde_json::json!({
            "path": path_str,
            "empty_directories": empties,
            "count": empties.len(),
            "truncated": truncated,
        }))
        .unwrap_or_else(|error| format!("序列化结果失败：{error}"))
    } else {
        let mut text = String::new();
        if empties.is_empty() {
            text.push_str("没有空目录。\n");
        } else {
            for directory in &empties {
                let _ = writeln!(text, "{directory}");
            }
        }
        let _ = writeln!(text, "\n共 {} 个空目录", empties.len());
        text
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_find_empty_directories",
        path = %path_str,
        count = empties.len(),
        truncated,
        duration_ms,
        "查找空目录完毕"
    );

    tool_result(Value::String(output), false)
}
