//! `builtin_unzip_file`：解压 ZIP 到目标目录。
//!
//! 语义对齐上游（③）：`unzip_file {zip_file, target_path}`。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、**zip-slip 防护**（`enclosed_name` 拒绝 `..` / 绝对路径
//! 条目）、`tool_audit` 审计。

use std::io::Read as _;
use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::FilesystemService;
use crate::builtin::filesystem::support::{failure, path_error, tool_result};

/// `builtin_unzip_file`
pub(crate) async fn unzip_file(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(zip_str) = arguments
        .get("zip_file")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "zip_file 必须是非空字符串");
    };
    let Some(target_str) = arguments
        .get("target_path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "target_path 必须是非空字符串");
    };

    let zip_resolved = match service.resolve(Path::new(zip_str)).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };
    let target_resolved = match service.resolve(Path::new(target_str)).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    let bytes = match zip_resolved.dir.read(&zip_resolved.rel) {
        Ok(bytes) => bytes,
        Err(error) => {
            let (code, message) = path_error("读取 ZIP", Path::new(zip_str), &error);
            return failure(code, message);
        }
    };

    let mut archive = match zip::ZipArchive::new(std::io::Cursor::new(bytes)) {
        Ok(archive) => archive,
        Err(error) => return failure("archive_error", format!("打开 ZIP 失败：{error}")),
    };

    let mut extracted = 0usize;
    let total = archive.len();
    for index in 0..total {
        let mut entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(error) => return failure("archive_error", format!("读取 ZIP 项失败：{error}")),
        };

        // zip-slip 防护：拒绝 `..` / 绝对路径条目。
        let Some(entry_rel) = entry.enclosed_name().map(|path| path.to_path_buf()) else {
            return failure(
                "archive_error",
                format!("ZIP 内含不安全路径，拒绝解压：{}", entry.name()),
            );
        };
        // ZIP 内是**相对条目名**，必须拼到目标目录的相对路径上再操作，
        // 否则会写到沙箱根而不是 target_path。
        let target_rel = target_resolved.rel.join(&entry_rel);

        if entry.is_dir() {
            if let Err(error) = target_resolved.dir.create_dir_all(&target_rel) {
                let (code, message) = path_error("创建目录", &target_rel, &error);
                return failure(code, message);
            }
            continue;
        }

        if let Some(parent) = target_rel.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(error) = target_resolved.dir.create_dir_all(parent) {
                    let (code, message) = path_error("创建目录", parent, &error);
                    return failure(code, message);
                }
            }
        }

        let mut content = Vec::new();
        if let Err(error) = entry.read_to_end(&mut content) {
            return failure("archive_error", format!("解压内容失败：{error}"));
        }
        if let Err(error) = target_resolved.dir.write(&target_rel, &content) {
            let (code, message) = path_error("写出文件", &target_rel, &error);
            return failure(code, message);
        }
        extracted += 1;
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_unzip_file",
        zip_file = %zip_str,
        target_path = %target_str,
        extracted,
        duration_ms,
        "解压完毕"
    );

    tool_result(
        Value::String(format!(
            "已解压 {zip_str} → {target_str}（{extracted} 个文件，共 {total} 项）"
        )),
        false,
    )
}
