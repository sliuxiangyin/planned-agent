//! `builtin_directory_tree`：以 JSON 结构返回递归目录树。
//!
//! 语义对齐上游（③）：每项 `{name, type, children?}`，`type` 为 `file` / `directory`，
//! 目录恒有 `children`（可为空）。`max_depth` 限深，超限时**不再下钻**。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、**条目上限与深度上限**（防爆）、`tool_audit` 审计。

use std::path::Path;
use std::time::Instant;

use cap_std::fs::Dir;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::FilesystemService;
use crate::builtin::filesystem::support::{failure, path_error, tool_result};

/// 单次遍历的条目上限（① 防爆）。
const MAX_TREE_ENTRIES: usize = 10_000;
/// 深度上限（① 防爆）：即使调用方不传 `max_depth` 也不会无限下钻。
const MAX_TREE_DEPTH: usize = 32;

/// `builtin_directory_tree`
pub(crate) async fn directory_tree(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let requested_depth = arguments
        .get("max_depth")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let depth_limit = match requested_depth {
        Some(depth) => depth.min(MAX_TREE_DEPTH),
        None => MAX_TREE_DEPTH,
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
    let mut counter = 0usize;
    let mut truncated = false;
    let (entries, reached_max_depth) = match build_tree(
        &root_dir,
        Path::new("."),
        0,
        depth_limit,
        &mut counter,
        &mut truncated,
    ) {
        Ok(value) => value,
        Err((code, message)) => return failure(code, message),
    };

    if counter == 0 {
        return failure(
            "internal_error",
            format!("「{path_str}」下没有任何条目（空目录或不可读）"),
        );
    }

    let mut payload = json!({ "path": path_str, "tree": entries });
    if let Some(object) = payload.as_object_mut() {
        if reached_max_depth || truncated {
            object.insert(
                "warning".to_string(),
                Value::String(
                    "输出不完整：超出 max_depth 的子目录被跳过，或条目数到达上限。".to_string(),
                ),
            );
        }
    }
    let text = match serde_json::to_string_pretty(&payload) {
        Ok(text) => text,
        Err(error) => return failure("internal_error", format!("序列化目录树失败：{error}")),
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_directory_tree",
        path = %path_str,
        entries = counter,
        max_depth = depth_limit,
        truncated,
        duration_ms,
        "目录树完毕"
    );

    tool_result(Value::String(text), false)
}

/// 递归构造目录树。返回 `(条目 JSON 数组, 是否触及深度上限)`。
///
/// `rel` 是相对 `dir` 句柄的路径；`depth` 为当前深度（根为 0）。
fn build_tree(
    dir: &Dir,
    rel: &Path,
    depth: usize,
    depth_limit: usize,
    counter: &mut usize,
    truncated: &mut bool,
) -> Result<(Vec<Value>, bool), (&'static str, String)> {
    let iterator = match dir.read_dir(rel) {
        Ok(iterator) => iterator,
        Err(error) => return Err(path_error("遍历目录", rel, &error)),
    };

    let mut children: Vec<cap_std::fs::DirEntry> = Vec::new();
    for item in iterator {
        match item {
            Ok(entry) => children.push(entry),
            Err(error) => return Err(path_error("遍历目录", rel, &error)),
        }
    }
    children.sort_by_key(|entry| entry.file_name());

    let mut nodes = Vec::new();
    let mut reached = false;

    for entry in children {
        if *counter >= MAX_TREE_ENTRIES {
            *truncated = true;
            break;
        }
        *counter += 1;

        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

        if !is_dir {
            nodes.push(json!({ "name": name, "type": "file" }));
            continue;
        }

        // 目录：达到深度上限则不再下钻（仍保留空 children）。
        if depth + 1 >= depth_limit {
            reached = true;
            nodes.push(json!({ "name": name, "type": "directory", "children": [] }));
            continue;
        }

        let child_rel = rel.join(&name);
        let (sub_nodes, sub_reached) =
            build_tree(dir, &child_rel, depth + 1, depth_limit, counter, truncated)?;
        if sub_reached {
            reached = true;
        }
        nodes.push(json!({ "name": name, "type": "directory", "children": sub_nodes }));
    }

    Ok((nodes, reached))
}
