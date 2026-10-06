//! 搜索与目录树（对齐上游 `src/fs_service/search/`）。

pub(crate) mod content;
pub(crate) mod files;
pub(crate) mod grep;
pub(crate) mod tree;

use std::path::PathBuf;

use cap_std::fs::Dir;

use crate::builtin::filesystem::support::path_error;

/// 单次遍历的条目上限（① 防爆）。
pub(crate) const MAX_WALK_ENTRIES: usize = 50_000;

/// 遍历到的一个条目。
pub(crate) struct WalkItem {
    /// 相对搜索根的路径。
    pub rel: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

/// 深度优先遍历 `root_dir` 下的全部条目（相对路径，含目录自身）。
///
/// 返回 `(条目, 是否因条目上限截断)`；错误落成契约错误码。
pub(crate) fn walk_all(
    root_dir: &Dir,
    visited: &mut usize,
    truncated: &mut bool,
) -> Result<Vec<WalkItem>, (&'static str, String)> {
    let mut items = Vec::new();
    let mut stack = vec![PathBuf::from(".")];

    while let Some(current) = stack.pop() {
        let iterator = root_dir
            .read_dir(&current)
            .map_err(|error| path_error("遍历目录", &current, &error))?;

        for entry in iterator {
            let entry = entry.map_err(|error| path_error("遍历目录", &current, &error))?;

            if *visited >= MAX_WALK_ENTRIES {
                *truncated = true;
                return Ok(items);
            }
            *visited += 1;

            let name = entry.file_name();
            let child = if current.as_os_str() == "." {
                PathBuf::from(&name)
            } else {
                current.join(&name)
            };
            let is_dir = entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false);
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            if is_dir {
                stack.push(child.clone());
            }
            items.push(WalkItem {
                rel: child,
                is_dir,
                size,
            });
        }
    }

    Ok(items)
}

/// 把相对路径转成 `/` 分隔的字符串（跨平台稳定，便于 glob 匹配与展示）。
pub(crate) fn rel_to_slash(rel: &std::path::Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}
