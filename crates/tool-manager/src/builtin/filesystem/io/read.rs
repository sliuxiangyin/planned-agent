//! 读取类工具：`builtin_read_text_file` / `builtin_read_file_lines`。
//!
//! 语义对齐上游 `src/fs_service/io/read.rs`（③）：
//! - `read_text_file` 返回**裸文本**（默认无行号；`with_line_numbers` 为 true 时行前缀 `{:>6} | `）；
//! - `read_file_lines` 是 **0-based** `offset` + 可选 `limit`。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、BOM/编码探测、二进制拒读、64 MiB 上限、`tool_audit` 审计。

use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::{FilesystemService, Resolved};
use crate::builtin::filesystem::support::{
    TextEncoding, decode, failure, looks_binary, path_error, sniff_encoding, string_array,
    tool_result,
};

/// 单文件「全文入内存」的硬上限（①）。
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// 行号列宽度：与上游 `format!("{:>6} | {}", ...)` 一致（③）。
const LINE_NUMBER_WIDTH: usize = 6;

/// `builtin_read_text_file`
pub(crate) async fn read_text_file(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let with_line_numbers = arguments
        .get("with_line_numbers")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let path = Path::new(path_str);

    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let bytes = match resolved.dir.read(&resolved.rel) {
        Ok(bytes) => bytes,
        Err(error) => {
            let (code, message) = path_error("读取文件", path, &error);
            return failure(code, message);
        }
    };

    if bytes.len() as u64 > MAX_FILE_BYTES {
        return failure(
            "content_too_large",
            format!(
                "「{path_str}」大小 {} 字节，超过单次读取上限 {MAX_FILE_BYTES} 字节；请改用 builtin_read_file_lines 分批读取",
                bytes.len()
            ),
        );
    }

    let text = match decode_text(&bytes, path_str) {
        Ok(text) => text,
        Err(result) => return result,
    };

    let total_lines = text.lines().count();
    let output = if with_line_numbers {
        text.lines()
            .enumerate()
            .map(|(index, line)| {
                format!("{:>width$} | {line}", index + 1, width = LINE_NUMBER_WIDTH)
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        text
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_read_text_file",
        path = %path_str,
        bytes = bytes.len(),
        total_lines,
        with_line_numbers,
        duration_ms,
        "读取文本文件完毕"
    );

    tool_result(Value::String(output), false)
}

/// `builtin_read_file_lines`
pub(crate) async fn read_file_lines(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let Some(offset) = arguments.get("offset").and_then(Value::as_u64) else {
        return failure(
            "invalid_arguments",
            "offset 必须是 >= 0 的整数（0-based 起始行号）",
        );
    };
    let limit = arguments.get("limit").and_then(Value::as_u64);
    let path = Path::new(path_str);

    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let bytes = match resolved.dir.read(&resolved.rel) {
        Ok(bytes) => bytes,
        Err(error) => {
            let (code, message) = path_error("读取文件", path, &error);
            return failure(code, message);
        }
    };

    if bytes.len() as u64 > MAX_FILE_BYTES {
        return failure(
            "content_too_large",
            format!(
                "「{path_str}」大小 {} 字节，超过单次读取上限 {MAX_FILE_BYTES} 字节；请缩小行范围或用其它手段",
                bytes.len()
            ),
        );
    }

    let text = match decode_text(&bytes, path_str) {
        Ok(text) => text,
        Err(result) => return result,
    };

    let lines: Vec<&str> = text.lines().collect();
    let start = (offset as usize).min(lines.len());
    let end = match limit {
        Some(limit) => start.saturating_add(limit as usize).min(lines.len()),
        None => lines.len(),
    };
    let selected = end - start;
    let output = lines[start..end].join("\n");

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_read_file_lines",
        path = %path_str,
        offset,
        limit,
        total_lines = lines.len(),
        selected,
        duration_ms,
        "按行读取完毕"
    );

    tool_result(Value::String(output), false)
}

/// 解码字节为文本。
///
/// `Err` 分支直接是**契约失败结果**（二进制 / 编码失败），调用方 `return` 即可。
/// 二进制判定只在按 UTF-8 解释时做：带 BOM 的 UTF-16 文本处处是 NUL，会被误杀。
fn decode_text(bytes: &[u8], path_str: &str) -> Result<String, ToolResult> {
    let (encoding, bom_len) = match sniff_encoding(bytes, "auto") {
        Ok(value) => value,
        Err(message) => return Err(failure("invalid_arguments", message)),
    };
    let body = &bytes[bom_len..];

    if encoding == TextEncoding::Utf8 && looks_binary(body) {
        return Err(failure(
            "binary_file",
            format!("「{path_str}」是二进制文件（开头 8 KiB 内含 NUL），本工具不读取二进制"),
        ));
    }

    decode(body, encoding).map_err(|reason| {
        failure(
            "invalid_encoding",
            format!("「{path_str}」{reason}；本工具按 UTF-8 / BOM 解码，其它编码请先转码"),
        )
    })
}

// ── 头尾预览 / 批量读取 / 媒体 ────────────────────────────────────────────

use base64::Engine as _;

/// 单次行数上限（① 防爆）。
const MAX_LINES: usize = 10_000;
/// 多文件读取的总输出上限（① 防爆）。
const MAX_MULTI_READ_BYTES: usize = 1024 * 1024;
/// 媒体单文件上限（① 防爆）。
const MAX_MEDIA_BYTES: u64 = 5 * 1024 * 1024;

/// 读取并按 BOM 解码为文本（编码 / 二进制 / 上限规则同 `read_text_file`）。
///
/// 错误直接是契约失败结果，调用方 `return` 即可。
fn read_text_of(resolved: &Resolved, path_str: &str) -> Result<String, ToolResult> {
    let bytes = match resolved.dir.read(&resolved.rel) {
        Ok(bytes) => bytes,
        Err(error) => {
            let (code, message) = path_error("读取文件", Path::new(path_str), &error);
            return Err(failure(code, message));
        }
    };
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(failure(
            "content_too_large",
            format!(
                "「{path_str}」大小 {} 字节，超过上限 {MAX_FILE_BYTES} 字节",
                bytes.len()
            ),
        ));
    }
    decode_text(&bytes, path_str)
}

/// `builtin_head_file`
pub(crate) async fn head_file(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let Some(lines) = arguments.get("lines").and_then(Value::as_u64) else {
        return failure("invalid_arguments", "lines 必须是 >= 0 的整数");
    };
    let lines = (lines as usize).min(MAX_LINES);
    let path = Path::new(path_str);

    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };
    let text = match read_text_of(&resolved, path_str) {
        Ok(text) => text,
        Err(result) => return result,
    };

    let output = text.lines().take(lines).collect::<Vec<_>>().join("\n");

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(target: "tool_audit", tool = "builtin_head_file", path = %path_str, lines, duration_ms, "读取文件头完毕");

    tool_result(Value::String(output), false)
}

/// `builtin_tail_file`
pub(crate) async fn tail_file(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let Some(lines) = arguments.get("lines").and_then(Value::as_u64) else {
        return failure("invalid_arguments", "lines 必须是 >= 0 的整数");
    };
    let lines = (lines as usize).min(MAX_LINES);
    let path = Path::new(path_str);

    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };
    let text = match read_text_of(&resolved, path_str) {
        Ok(text) => text,
        Err(result) => return result,
    };

    // 简化实现：读全文后取尾部 N 行（上游是反向 chunk 读）。
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    let output = all[start..].join("\n");

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(target: "tool_audit", tool = "builtin_tail_file", path = %path_str, lines, total_lines = all.len(), duration_ms, "读取文件尾完毕");

    tool_result(Value::String(output), false)
}

/// `builtin_read_multiple_text_files`
pub(crate) async fn read_multiple_text_files(
    service: &FilesystemService,
    arguments: &Value,
) -> ToolResult {
    let started = Instant::now();

    let paths = string_array(arguments.get("paths"));
    if paths.is_empty() {
        return failure("invalid_arguments", "paths 必须是非空字符串数组");
    }

    let mut output = String::new();
    let mut total = 0usize;
    let mut truncated = false;
    let mut ok_count = 0usize;

    for path_str in &paths {
        let resolved = match service.resolve(Path::new(path_str)).await {
            Ok(resolved) => resolved,
            Err(error) => {
                output.push_str(&format!("=== {path_str} ===\n[{}] {}\n\n", error.code(), error.message()));
                continue;
            }
        };
        match read_text_of(&resolved, path_str) {
            Ok(text) => {
                if total + text.len() > MAX_MULTI_READ_BYTES {
                    truncated = true;
                    output.push_str("（达到总输出上限，后续文件省略）\n");
                    break;
                }
                total += text.len();
                ok_count += 1;
                output.push_str(&format!("=== {path_str} ===\n"));
                output.push_str(&text);
                output.push('\n');
            }
            Err(result) => {
                let code = result
                    .content
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("internal_error");
                let message = result
                    .content
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                output.push_str(&format!("=== {path_str} ===\n[{code}] {message}\n\n"));
            }
        }
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_read_multiple_text_files",
        requested = paths.len(),
        read = ok_count,
        truncated,
        duration_ms,
        "批量读取文本文件完毕"
    );

    tool_result(Value::String(output), false)
}

/// `builtin_read_media_file`
pub(crate) async fn read_media_file(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let max_bytes = arguments
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_MEDIA_BYTES);
    let path = Path::new(path_str);

    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let bytes = match resolved.dir.read(&resolved.rel) {
        Ok(bytes) => bytes,
        Err(error) => {
            let (code, message) = path_error("读取文件", path, &error);
            return failure(code, message);
        }
    };
    if bytes.len() as u64 > max_bytes {
        return failure(
            "content_too_large",
            format!(
                "「{path_str}」大小 {} 字节，超过媒体读取上限 {max_bytes} 字节",
                bytes.len()
            ),
        );
    }

    let mime = infer::get(&bytes)
        .map(|kind| kind.mime_type().to_string())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(target: "tool_audit", tool = "builtin_read_media_file", path = %path_str, mime = %mime, bytes = bytes.len(), duration_ms, "读取媒体文件完毕");

    tool_result(
        serde_json::json!({
            "path": path_str,
            "mime_type": mime,
            "size_bytes": bytes.len(),
            "data_base64": data,
        }),
        false,
    )
}

/// `builtin_read_multiple_media_files`
pub(crate) async fn read_multiple_media_files(
    service: &FilesystemService,
    arguments: &Value,
) -> ToolResult {
    let started = Instant::now();

    let paths = string_array(arguments.get("paths"));
    if paths.is_empty() {
        return failure("invalid_arguments", "paths 必须是非空字符串数组");
    }
    let max_bytes = arguments
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_MEDIA_BYTES);

    let mut items = Vec::new();
    for path_str in &paths {
        let resolved = match service.resolve(Path::new(path_str)).await {
            Ok(resolved) => resolved,
            Err(error) => {
                items.push(serde_json::json!({
                    "path": path_str,
                    "error": error.code(),
                    "message": error.message(),
                }));
                continue;
            }
        };
        match resolved.dir.read(&resolved.rel) {
            Ok(bytes) if bytes.len() as u64 <= max_bytes => {
                let mime = infer::get(&bytes)
                    .map(|kind| kind.mime_type().to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());
                items.push(serde_json::json!({
                    "path": path_str,
                    "mime_type": mime,
                    "size_bytes": bytes.len(),
                    "data_base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
                }));
            }
            Ok(bytes) => items.push(serde_json::json!({
                "path": path_str,
                "error": "content_too_large",
                "message": format!("{} 字节超过上限 {max_bytes} 字节", bytes.len()),
            })),
            Err(error) => {
                let (code, message) = path_error("读取文件", Path::new(path_str), &error);
                items.push(serde_json::json!({
                    "path": path_str,
                    "error": code,
                    "message": message,
                }));
            }
        }
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(target: "tool_audit", tool = "builtin_read_multiple_media_files", requested = paths.len(), duration_ms, "批量读取媒体文件完毕");

    tool_result(Value::Array(items), false)
}
