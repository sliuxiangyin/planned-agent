//! `builtin_zip_files` / `builtin_zip_directory`。
//!
//! 语义对齐上游（③）：
//! - `zip_files {input_files, target_zip_file}`：把给定文件打进一个 ZIP；
//! - `zip_directory {input_directory, pattern?, target_zip_file}`：按 glob 打包目录（pattern 默认 `**/*`）。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、**先在内存里打包再原子落盘**、`tool_audit` 审计。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use glob_match::glob_match;
use serde_json::Value;

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::FilesystemService;
use crate::builtin::filesystem::search::{rel_to_slash, walk_all};
use crate::builtin::filesystem::support::{failure, path_error, string_array, tool_result};

use super::super::io::write::atomic_write_in;

/// `builtin_zip_files`
pub(crate) async fn zip_files(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let input_files = string_array(arguments.get("input_files"));
    let Some(target_str) = arguments
        .get("target_zip_file")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "target_zip_file 必须是非空字符串");
    };
    if input_files.is_empty() {
        return failure("invalid_arguments", "input_files 必须是非空字符串数组");
    }

    let target = match service.resolve(Path::new(target_str)).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        for file in &input_files {
            let resolved = match service.resolve(Path::new(file)).await {
                Ok(resolved) => resolved,
                Err(error) => return error.into_tool_result(),
            };
            let bytes = match resolved.dir.read(&resolved.rel) {
                Ok(bytes) => bytes,
                Err(error) => {
                    let (code, message) = path_error("读取文件", Path::new(file), &error);
                    return failure(code, message);
                }
            };
            let name = PathBuf::from(file)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.clone());
            if let Err(error) = writer.start_file(name, options) {
                return failure("archive_error", format!("写入 ZIP 项失败：{error}"));
            }
            if let Err(error) = writer.write_all(&bytes) {
                return failure("archive_error", format!("写入 ZIP 内容失败：{error}"));
            }
        }

        if let Err(error) = writer.finish() {
            return failure("archive_error", format!("完成 ZIP 失败：{error}"));
        }
    }

    let archive_bytes = buffer.get_ref().clone();
    if let Err(error) = atomic_write_in(&target, &archive_bytes) {
        let (code, message) = path_error("写入 ZIP", Path::new(target_str), &error);
        return failure(code, message);
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_zip_files",
        target_zip_file = %target_str,
        files = input_files.len(),
        bytes = archive_bytes.len(),
        duration_ms,
        "打包文件完毕"
    );

    tool_result(
        Value::String(format!(
            "已创建 ZIP：{target_str}（{} 个文件，{} 字节）",
            input_files.len(),
            archive_bytes.len()
        )),
        false,
    )
}

/// `builtin_zip_directory`
pub(crate) async fn zip_directory(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(directory_str) = arguments
        .get("input_directory")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "input_directory 必须是非空字符串");
    };
    let Some(target_str) = arguments
        .get("target_zip_file")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "target_zip_file 必须是非空字符串");
    };
    let pattern = arguments
        .get("pattern")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("**/*")
        .to_lowercase();

    let base = match service.resolve(Path::new(directory_str)).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };
    let target = match service.resolve(Path::new(target_str)).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let base_dir = match base.open_dir() {
        Ok(dir) => dir,
        Err(error) => {
            let (code, message) = path_error("打开目录", Path::new(directory_str), &error);
            return failure(code, message);
        }
    };
    let mut visited = 0usize;
    let mut walk_truncated = false;
    let items = match walk_all(&base_dir, &mut visited, &mut walk_truncated) {
        Ok(items) => items,
        Err((code, message)) => return failure(code, message),
    };

    let mut buffer = std::io::Cursor::new(Vec::new());
    let mut count = 0usize;
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        for item in &items {
            if item.is_dir {
                continue;
            }
            let rel_slash = rel_to_slash(&item.rel);
            if !glob_match(&pattern, &rel_slash.to_lowercase()) {
                continue;
            }
            let bytes = match base_dir.read(&item.rel) {
                Ok(bytes) => bytes,
                Err(error) => {
                    let (code, message) = path_error("读取文件", &item.rel, &error);
                    return failure(code, message);
                }
            };
            if let Err(error) = writer.start_file(rel_slash, options) {
                return failure("archive_error", format!("写入 ZIP 项失败：{error}"));
            }
            if let Err(error) = writer.write_all(&bytes) {
                return failure("archive_error", format!("写入 ZIP 内容失败：{error}"));
            }
            count += 1;
        }

        if let Err(error) = writer.finish() {
            return failure("archive_error", format!("完成 ZIP 失败：{error}"));
        }
    }

    let archive_bytes = buffer.get_ref().clone();
    if let Err(error) = atomic_write_in(&target, &archive_bytes) {
        let (code, message) = path_error("写入 ZIP", Path::new(target_str), &error);
        return failure(code, message);
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_zip_directory",
        input_directory = %directory_str,
        target_zip_file = %target_str,
        files = count,
        truncated = walk_truncated,
        duration_ms,
        "打包目录完毕"
    );

    tool_result(
        Value::String(format!(
            "已创建 ZIP：{target_str}（{count} 个文件，{} 字节）",
            archive_bytes.len()
        )),
        false,
    )
}
