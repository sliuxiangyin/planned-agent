//! 启动命令解析：在 spawn 子进程**之前**，按当前系统环境把命令名解析为真实可执行文件路径。
//!
//! ## 为什么需要
//!
//! - **Windows 特有坑**：`npx` / `tsx` / `uvx` 等 npm/python shim 实际是 `npx.cmd`
//!   （批处理脚本），并没有 `npx.exe`。`std::process::Command::new("npx")` 在 Windows
//!   上只按 `npx` → `npx.exe` 查找（`CreateProcess` 不解析 `.cmd`/`.bat` 后缀），
//!   即使 npx 已安装并位于 PATH 中也会报 `program not found`，对用户毫无指引。
//! - **通用体验**：spawn 阶段的 io::Error 对"命令不存在"的表述模糊，用户无法区分是
//!   没安装、没配 PATH、还是拼写错误。
//!
//! 这里在 spawn 前主动解析：
//! 1. **找到** → 返回可执行文件完整路径。Windows 下把 `npx` 解析成 `npx.cmd` 的完整
//!    路径后，`std::process::Command` 识别 `.cmd`/`.bat` 扩展名会自动经 `cmd.exe`
//!    包装执行，因此 Windows 下也能正常拉起。
//! 2. **找不到** → 返回中文错误"命令不存在: {command}"，由调用方包装为
//!    [`crate::client`] 的 `ConnectionError::Spawn` 展示给用户。

use std::path::{Path, PathBuf};

/// PATH 环境变量分隔符（Windows 用 `;`，类 Unix 用 `:`）。
#[cfg(windows)]
const PATH_SEP: char = ';';
#[cfg(not(windows))]
const PATH_SEP: char = ':';

/// Windows 下按可执行性顺序尝试的后缀（PATHEXT 常见集合）。
///
/// `.cmd`/`.bat` 排在 `.exe` 之后：std 对 `.cmd`/`.bat` 会经 `cmd.exe` 包装执行，
/// 两者都能被 `Command::new` 正常拉起。
#[cfg(windows)]
const WINDOWS_EXE_SUFFIXES: &[&str] = &[".exe", ".cmd", ".bat", ".com"];

/// 解析启动命令，返回系统中真实存在的可执行文件路径。
///
/// - 命令含路径分隔符（如 `C:\tools\node.exe`、`./bin/srv`、`/usr/bin/node`）：
///   视为显式路径，直接检查存在性（Windows 下无扩展名时补充可执行后缀）；
/// - 否则视为命令名，按 PATH 目录依次查找（Windows 附带常见可执行后缀）。
///
/// 失败时返回中文错误消息（含"命令不存在"）。
pub fn resolve_command(command: &str) -> Result<PathBuf, String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err("命令不存在: （空命令）".to_string());
    }

    let path = Path::new(trimmed);
    if path_has_separator(trimmed) {
        return match check_explicit_path(path) {
            Some(p) => Ok(p),
            None => Err(not_found_msg(trimmed)),
        };
    }

    if let Some(found) = search_path(trimmed) {
        return Ok(found);
    }
    Err(not_found_msg(trimmed))
}

/// 命令文本是否含路径分隔符（决定按"显式路径"还是"命令名"解析）。
#[cfg(windows)]
fn path_has_separator(cmd: &str) -> bool {
    cmd.contains('\\') || cmd.contains('/') || cmd.contains(':')
}

#[cfg(not(windows))]
fn path_has_separator(cmd: &str) -> bool {
    cmd.contains('/')
}

/// 检查显式路径（含分隔符的命令）。
#[cfg(windows)]
fn check_explicit_path(path: &Path) -> Option<PathBuf> {
    // 已带可执行后缀 → 直接检查文件存在性
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext = format!(".{}", ext.to_ascii_lowercase());
        return WINDOWS_EXE_SUFFIXES
            .contains(&ext.as_str())
            .then(|| path.is_file().then(|| path.to_path_buf()))
            .flatten();
    }
    // 无扩展名 → 依次补充可执行后缀尝试
    for suffix in WINDOWS_EXE_SUFFIXES {
        let mut candidate = path.as_os_str().to_os_string();
        candidate.push(suffix);
        let candidate = PathBuf::from(candidate);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(not(windows))]
fn check_explicit_path(path: &Path) -> Option<PathBuf> {
    is_executable_file(path).then(|| path.to_path_buf())
}

/// 按 PATH 查找命令名。
///
/// 注意：**不能**先命中"裸名文件"——nodejs 目录下存在无扩展名的 `npx` sh 脚本，
/// 它不是 PE 可执行文件，`CreateProcess` 无法直接执行；返回它仍会导致
/// "program not found"。必须落到 `.exe` / `.cmd` 等带后缀的真实可执行文件。
#[cfg(windows)]
fn search_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    // 命令名自身已带可执行后缀（如 "npx.cmd"）→ 按原名直接检查
    let name_has_exe_ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let ext = format!(".{}", e.to_ascii_lowercase());
            WINDOWS_EXE_SUFFIXES.contains(&ext.as_str())
        })
        .unwrap_or(false);
    for dir in path_var.split(PATH_SEP).filter(|d| !d.is_empty()) {
        let dir = Path::new(dir);
        if name_has_exe_ext {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
            continue;
        }
        for suffix in WINDOWS_EXE_SUFFIXES {
            let mut candidate = dir.join(name).as_os_str().to_os_string();
            candidate.push(suffix);
            let candidate = PathBuf::from(candidate);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn search_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in path_var.split(PATH_SEP).filter(|d| !d.is_empty()) {
        let candidate = Path::new(dir).join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// 类 Unix 下的可执行文件判定（存在 + 有任一执行位）。
#[cfg(not(windows))]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && path
            .metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

/// "命令不存在"错误消息（分平台提示，帮助用户定位）。
#[cfg(windows)]
fn not_found_msg(command: &str) -> String {
    format!(
        "命令不存在: {command}（已按 PATH 搜索 {command}.exe/.cmd/.bat/.com，均未找到。\
         请确认已安装并加入 PATH；Windows 下 npm/python 命令通常以 .cmd 结尾，\
         如 npx 实际为 npx.cmd）"
    )
}

#[cfg(not(windows))]
fn not_found_msg(command: &str) -> String {
    format!(
        "命令不存在: {command}（已按 PATH 搜索，未找到可执行文件。请确认已安装并加入 PATH）"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_command_rejected() {
        for bad in ["", "   "] {
            let err = resolve_command(bad).unwrap_err();
            assert!(err.contains("命令不存在"), "got: {err}");
        }
    }

    #[test]
    fn missing_command_reports_not_found() {
        let err = resolve_command("definitely_not_a_real_cmd_xyz").unwrap_err();
        assert!(err.contains("命令不存在"), "got: {err}");
        assert!(err.contains("definitely_not_a_real_cmd_xyz"), "got: {err}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_resolves_system_command() {
        // Windows 系统必有 cmd.exe（C:\Windows\System32\cmd.exe）
        let resolved = resolve_command("cmd").expect("cmd 应能在 PATH 中解析");
        let lower = resolved.to_string_lossy().to_ascii_lowercase();
        assert!(
            lower.ends_with("cmd.exe"),
            "应解析为 cmd.exe，实际: {lower}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_resolves_npm_cmd_shim_when_nodejs_installed() {
        // nodejs 安装在 PATH 时，npx 应解析为 npx.cmd（本仓库开发机场景）
        let has_nodejs = std::env::var("PATH")
            .map(|p| p.to_ascii_lowercase().contains("nodejs"))
            .unwrap_or(false);
        if has_nodejs {
            let resolved = resolve_command("npx").expect("PATH 含 nodejs 时 npx 应能解析");
            let lower = resolved.to_string_lossy().to_ascii_lowercase();
            assert!(
                lower.ends_with(".cmd") || lower.ends_with(".exe"),
                "npx 应解析为 npx.cmd/npx.exe，实际: {lower}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_explicit_path_without_ext_gets_suffix() {
        // 显式路径但缺扩展名：应补 .exe 后命中（用系统目录的 cmd 验证）
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let probe = format!("{}\\System32\\cmd", system_root);
        let resolved = resolve_command(&probe).expect("cmd 无扩展名显式路径应补 .exe 命中");
        assert!(
            resolved.to_string_lossy().to_ascii_lowercase().ends_with("cmd.exe"),
            "应补 .exe，实际: {}",
            resolved.to_string_lossy()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_resolves_shell_or_fails_cleanly() {
        // 类 Unix 系统通常有 sh（PATH 中）
        match resolve_command("sh") {
            Ok(p) => {
                let s = p.to_string_lossy();
                assert!(s.contains('/'), "应解析为绝对/相对路径，实际: {s}");
            }
            Err(e) => assert!(e.contains("命令不存在"), "got: {e}"),
        }
    }
}
