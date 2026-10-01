//! 写入类工具：`builtin_write_file` / `builtin_create_directory` / `builtin_move_file`。
//!
//! 语义对齐上游（③）：
//! - `write_file` 是 `{path, content}` **纯覆盖**（无 `mode` / `create_parents` / `ensure_newline`）；
//! - `create_directory` 支持多级嵌套；
//! - `move_file` **目标已存在则失败**（不覆盖）。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、**原子写**、10 MiB 上限、`tool_audit` 审计。

use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::{FilesystemService, Resolved};
use crate::builtin::filesystem::support::{content_hash, failure, path_error, tool_result};

/// 单次写入内容上限（①）。
const MAX_WRITE_BYTES: u64 = 10 * 1024 * 1024;

/// `builtin_write_file`
pub(crate) async fn write_file(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let Some(content) = arguments.get("content").and_then(Value::as_str) else {
        return failure("invalid_arguments", "content 必须是字符串");
    };
    let path = Path::new(path_str);

    if content.len() as u64 > MAX_WRITE_BYTES {
        return failure(
            "content_too_large",
            format!(
                "写入内容 {} 字节，超过上限 {MAX_WRITE_BYTES} 字节；请分块写入",
                content.len()
            ),
        );
    }

    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let bytes = content.as_bytes();
    if let Err(error) = atomic_write_in(&resolved, bytes) {
        let (code, message) = path_error("写入文件", path, &error);
        return failure(code, message);
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_write_file",
        path = %path_str,
        bytes_written = bytes.len(),
        content_hash = %content_hash(bytes),
        duration_ms,
        "写入文件完毕"
    );

    tool_result(Value::String(format!("已写入 {path_str}")), false)
}

/// `builtin_create_directory`
pub(crate) async fn create_directory(service: &FilesystemService, arguments: &Value) -> ToolResult {
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

    if let Err(error) = resolved.dir.create_dir_all(&resolved.rel) {
        let (code, message) = path_error("创建目录", path, &error);
        return failure(code, message);
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_create_directory",
        path = %path_str,
        duration_ms,
        "创建目录完毕"
    );

    tool_result(Value::String(format!("已创建目录 {path_str}")), false)
}

/// `builtin_move_file`
pub(crate) async fn move_file(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(source_str) = arguments
        .get("source")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "source 必须是非空字符串");
    };
    let Some(destination_str) = arguments
        .get("destination")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "destination 必须是非空字符串");
    };
    let source_path = Path::new(source_str);
    let destination_path = Path::new(destination_str);

    let source = match service.resolve(source_path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };
    let destination = match service.resolve(destination_path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    // ③ 上游语义：目标已存在则失败（不覆盖）。
    // 显式检查而非依赖 rename 的行为 —— cap-std 的 rename **支持覆盖**（见 tests/cap_std_contract.rs），
    // 直接 rename 会静默覆盖，与契约不符。
    if destination.dir.metadata(&destination.rel).is_ok() {
        return failure(
            "already_exists",
            format!("目标「{destination_str}」已存在；move_file 不覆盖已有文件/目录"),
        );
    }

    if let Err(error) = source.dir.rename(&source.rel, &destination.dir, &destination.rel) {
        let (code, message) = path_error("移动", source_path, &error);
        return failure(code, message);
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_move_file",
        source = %source_str,
        destination = %destination_str,
        duration_ms,
        "移动完毕"
    );

    tool_result(
        Value::String(format!("已移动 {source_str} → {destination_str}")),
        false,
    )
}

/// 在 cap-std 句柄内**原子写入**：同目录临时文件 → `rename` 覆盖。
///
/// `rename` 能覆盖已存在目标（cap-std，已由 `tests/cap_std_contract.rs` 验证，Windows 亦成立），
/// 故不必退回 `std::fs`。任何一步失败都清掉临时文件，**不留半截文件**。
///
/// 被 [`super::edit`] 复用。
pub(crate) fn atomic_write_in(resolved: &Resolved, bytes: &[u8]) -> std::io::Result<()> {
    let dir = &resolved.dir;
    let rel = &resolved.rel;
    let file_name = rel
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp_rel = rel.with_file_name(format!(".{file_name}.tmp.{}", uuid::Uuid::new_v4()));

    let result = (|| -> std::io::Result<()> {
        dir.write(&tmp_rel, bytes)?;
        dir.rename(&tmp_rel, dir, rel)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = dir.remove_file(&tmp_rel);
    }
    result
}
