//! filesystem 工具的**服务层**：cap-std 能力沙箱。
//!
//! 所有文件访问都先经 [`FilesystemService::resolve`] 落到某个允许目录的 cap-std 句柄上：
//! 越界（`..`、symlink 逃逸、不在白名单内）一律拒绝。隔离由**句柄相对操作**在 OS 层承担，
//! 不靠路径字符串比较 —— 详见 `docs/planned-agent/filesystem-tools-rewrite.md` §6。
//!
//! 语义参考 rust-mcp-filesystem `src/fs_service/core.rs`（MIT），按需重写，不引其依赖。

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use tokio::sync::RwLock;

use super::support::{FsError, FsResult};

/// 一个允许目录：cap-std 句柄 + 展示用绝对路径。
pub(crate) struct AllowedDir {
    /// 规范化、去 verbatim 的绝对路径，用于前缀匹配与展示。
    pub path: PathBuf,
    /// cap-std 目录句柄：只能访问自身子树。
    pub dir: Dir,
}

/// [`FilesystemService::resolve`] 的结果。
///
/// 调用方拿 `dir` + `rel` 做相对操作，`display` 只用于展示与错误消息。
#[derive(Debug)]
pub(crate) struct Resolved {
    /// 受限目录句柄。
    pub dir: Dir,
    /// 相对该句柄的路径。
    pub rel: PathBuf,
    /// 规范化绝对路径（展示用）。
    pub display: PathBuf,
}

impl Resolved {
    /// 打开 `rel` 指向的目录，得到一个**以它自身为根**的句柄。
    ///
    /// 需要「以某个子目录为遍历根」的工具（`search_*` / `directory_tree` / `zip_directory`）
    /// 必须用它，否则遍历会从沙箱根开始，产出带多余前缀的相对路径。
    pub(crate) fn open_dir(&self) -> std::io::Result<Dir> {
        if self.rel.as_os_str() == "." {
            self.dir.try_clone()
        } else {
            self.dir.open_dir(&self.rel)
        }
    }
}

/// filesystem 服务：持有允许目录白名单。
pub(crate) struct FilesystemService {
    allowed: RwLock<Arc<Vec<AllowedDir>>>,
}

impl FilesystemService {
    /// 用允许目录构造。空列表合法，但此后一切 `resolve` 都会被拒（安全默认）。
    pub(crate) fn try_new(roots: &[PathBuf]) -> FsResult<Self> {
        let mut dirs = Vec::with_capacity(roots.len());
        for root in roots {
            let canonical = root.canonicalize().map_err(|error| {
                FsError::InvalidArgs(format!(
                    "允许目录「{}」不可用：{error}（请确认它存在且可访问）",
                    root.display()
                ))
            })?;
            if !canonical.is_dir() {
                return Err(FsError::InvalidArgs(format!(
                    "允许目录「{}」不是目录",
                    root.display()
                )));
            }
            let dir = Dir::open_ambient_dir(&canonical, ambient_authority())?;
            dirs.push(AllowedDir {
                path: strip_verbatim_path(&canonical),
                dir,
            });
        }
        Ok(Self {
            allowed: RwLock::new(Arc::new(dirs)),
        })
    }

    /// 把请求路径解析成「受限句柄 + 相对路径」，越界即拒。
    pub(crate) async fn resolve(&self, requested: &Path) -> FsResult<Resolved> {
        let allowed = self.allowed.read().await.clone();
        if allowed.is_empty() {
            return Err(FsError::OutsideAllowed(
                "没有配置任何允许目录，拒绝一切文件访问".into(),
            ));
        }

        let absolute = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(FsError::from)?
                .join(requested)
        };

        let canonical = canonicalize_best_effort(&absolute)?;

        for dir in allowed.iter() {
            if let Ok(rel) = canonical.strip_prefix(&dir.path) {
                // 纵深防御：拒绝未解析的父目录分量。
                if rel.components().any(|c| matches!(c, Component::ParentDir)) {
                    return Err(FsError::OutsideAllowed(format!(
                        "路径包含未解析的父目录分量：{}",
                        canonical.display()
                    )));
                }
                // 请求的就是根目录自身时 `strip_prefix` 得到空路径。空路径在
                // `read_dir` / `create_dir_all` / `metadata` 上行为不一致（cap-std 会报
                // NotFound），统一归一为 `.`，让所有调用方都能安全地做相对操作。
                let rel = if rel.as_os_str().is_empty() {
                    Path::new(".").to_path_buf()
                } else {
                    rel.to_path_buf()
                };
                return Ok(Resolved {
                    dir: dir.dir.try_clone().map_err(FsError::from)?,
                    rel,
                    display: canonical,
                });
            }
        }

        Err(FsError::OutsideAllowed(format!(
            "访问被拒绝：{} 不在允许目录内（允许：{}）",
            absolute.display(),
            allowed
                .iter()
                .map(|d| d.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }
}

/// 去掉 Windows verbatim 前缀：`\\?\D:\a` → `D:\a`；`\\?\UNC\s\sh` → `\\s\sh`。
///
/// 用运行时判断而非 `#[cfg(windows)]`，保持本仓「零 cfg 分支」约定。
fn strip_verbatim_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// `canonicalize`；路径不存在时，对**最深的已存在祖先** canonicalize 再拼回后缀。
///
/// 这样「即将创建」的路径也能参与前缀匹配（实际边界仍由 cap-std 句柄承担）。
fn canonicalize_best_effort(path: &Path) -> FsResult<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Ok(strip_verbatim_path(&canonical));
    }

    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut ancestor = path;
    loop {
        if let Ok(canonical) = ancestor.canonicalize() {
            let mut result = strip_verbatim_path(&canonical);
            for part in suffix.iter().rev() {
                result.push(part);
            }
            return Ok(result);
        }
        let name = ancestor.file_name().ok_or_else(|| {
            FsError::NotFound(format!("无法解析路径：{}", path.display()))
        })?;
        suffix.push(name.to_os_string());
        ancestor = ancestor.parent().ok_or_else(|| {
            FsError::NotFound(format!("无法解析路径：{}", path.display()))
        })?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_verbatim_handles_windows_prefixes() {
        assert_eq!(
            strip_verbatim_path(Path::new(r"\\?\D:\a\b")),
            PathBuf::from(r"D:\a\b")
        );
        assert_eq!(
            strip_verbatim_path(Path::new(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\server\share")
        );
        assert_eq!(
            strip_verbatim_path(Path::new(r"D:\plain")),
            PathBuf::from(r"D:\plain")
        );
    }

    #[tokio::test]
    async fn resolve_rejects_paths_outside_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, b"secret").unwrap();

        let service = FilesystemService::try_new(std::slice::from_ref(&root)).unwrap();

        // 允许目录内：可以
        let inside = root.join("a.txt");
        std::fs::write(&inside, b"ok").unwrap();
        assert!(service.resolve(&inside).await.is_ok());

        // 目录外：拒绝
        let err = service.resolve(&outside).await.unwrap_err();
        assert_eq!(err.code(), "path_outside_allowed");
    }

    #[tokio::test]
    async fn resolve_rejects_everything_when_no_roots() {
        let service = FilesystemService::try_new(&[]).unwrap();
        let err = service.resolve(Path::new("a.txt")).await.unwrap_err();
        assert_eq!(err.code(), "path_outside_allowed");
    }
}
