//! 运行环境事实：把「本机是什么环境」探测出来。
//!
//! 目的：消灭「环境不确定 → 反复试探」这类弯路（如先跑 `uname` 再跑 `ver`、
//! 反复试 `python3` 与 `python`、试了 `go build` 才发现本机没装 go）。
//! 这些事实**可确定性探测**，不该由模型猜。
//!
//! 边界：本模块**只产出 [`RuntimeEnvironment`] 结构体**，且只做探测：
//! - **不**渲染成 prompt 文本 —— 怎么组织、要不要注入，由调用方决定；
//! - **不**给「该用什么命令」的建议（如「Windows 用 dir 不用 ls」）——
//!   那是使用方拿到 `os` 之后的策略，不是环境事实；
//! - **不认识** `AiClient` / 工具注册表 / 落库。
//!
//! 设计见 `docs/planned-agent/flexible-execution-improvements.md` §5.2。

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// 单个命令的版本探测超时（超时即判不可用，避免拖死服务启动）。
///
/// 给 3s 的余量：配上 `CREATE_NO_WINDOW` 之后版本命令通常 <100ms，
/// 但首次冷启动（杀软扫描 / 磁盘冷读）偶发变慢 —— 实测 1.5s 曾导致整批压线超时。
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Windows `CREATE_NO_WINDOW`：子进程不新建控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 版本原文保留的最大字符数（取首行）。
const VERSION_MAX_CHARS: usize = 60;

/// 默认探测名单：node / python / php / go / rust（cargo + rustc）。
///
/// `rust` 本身不是命令，落到 `cargo` / `rustc` 两个名字上；
/// `python` 与 `python3` 在 Windows 上可能只存在其一，故都探。
pub const DEFAULT_PROBE_NAMES: &[&str] = &[
    "node", "python", "python3", "php", "go", "cargo", "rustc",
];

/// 本机可执行环境：探测到的（带版本）/ 明确跑不起来的。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecutableProbe {
    /// 可用项：`(命令名, 版本首行)`；版本可能为 `None`（能跑但读不到版本）。
    pub available: Vec<(String, Option<String>)>,
    /// 不可用项 —— 显式告诉模型「别试」。
    pub missing: Vec<String>,
}

impl ExecutableProbe {
    /// 是否什么都没探到（用于「整段省略」）。
    pub fn is_empty(&self) -> bool {
        self.available.is_empty() && self.missing.is_empty()
    }
}

/// 本次执行的运行环境事实（全部可确定性探测）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEnvironment {
    /// `std::env::consts::OS`：`"windows"` / `"linux"` / `"macos"` …
    pub os: String,
    /// `std::env::consts::ARCH`：`"x86_64"` / `"aarch64"` …
    pub arch: String,
    /// `std::path::MAIN_SEPARATOR`。
    pub path_separator: char,
    /// 行尾显示名：`"CRLF"` / `"LF"`（给 prompt 看的，非实际字符）。
    pub line_ending: String,
    /// **优先使用的** shell 名 —— 按可用性探测得出，不是「系统默认解释器」。
    pub shell: Option<String>,
    /// 控制台输出编码。**当前恒为 `None`**：内核无法可靠探测（要问 Win32 API 或跑 `chcp`），
    /// 而工具层已在执行时把命令输出强制成 UTF-8，因此不必再知道原始代码页。
    pub console_encoding: Option<String>,
    /// 本机可执行环境（`detect()` 才会填）。
    #[serde(default)]
    pub executables: ExecutableProbe,
    /// 当前工作目录。
    ///
    /// **默认不进 prompt**：含用户名等路径信息，属「外发」隐私（见设计稿 §5.2.5）。
    pub working_dir: Option<String>,
    /// 宿主指定的**产出目录**：工具的落盘产出（截图 / 导出 / 保存）都该落在这里。
    ///
    /// **不是探测结果**：`detect*` 一律填 `None`，由宿主构造后用 [`Self::with_output_dir`]
    /// 注入（与 `notes` 同为「宿主补充的事实」）。
    ///
    /// **默认进 prompt**（与 `working_dir` 相反）：模型必须知道产出落哪，否则写出的文件
    /// 下游工具（沙箱根 = 该目录）读不到，只能靠来回搬运。见设计稿 §5.2.5 的例外说明。
    #[serde(default)]
    pub output_dir: Option<String>,
    /// 人工补充的其它事实（如「目标目录只读」「本机只有 python」）。
    pub notes: Option<String>,
    /// 探测时刻（RFC3339）；**只给 UI 看，不进 prompt**。
    pub probed_at: Option<String>,
}

impl RuntimeEnvironment {
    /// 完整探测：宿主事实 + 可执行环境（会 spawn 进程）。
    ///
    /// 应当在**服务启动时调用一次**（不是每次执行、也不是每步）。
    pub async fn detect() -> Self {
        let mut env = Self::detect_host();
        env.executables = probe_executables(DEFAULT_PROBE_NAMES).await;
        env.probed_at = Some(chrono::Utc::now().to_rfc3339());
        env
    }

    /// 只探测宿主事实（**不 spawn 任何进程**）—— 同步、零副作用，便于单测。
    pub fn detect_host() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            path_separator: std::path::MAIN_SEPARATOR,
            line_ending: if cfg!(windows) { "CRLF" } else { "LF" }.to_string(),
            shell: preferred_shell(),
            console_encoding: None,
            executables: ExecutableProbe::default(),
            working_dir: std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string()),
            output_dir: None,
            notes: None,
            probed_at: None,
        }
    }

    /// 注入**产出目录**（宿主侧用；`detect*` 不填此项）。
    ///
    /// 链式写法让调用点在「取快照那一刻」补上宿主配置派生的值，不必让 `core`
    /// 认识 GUI 的 `cache_root`：`env.snapshot().with_output_dir(app.flexible_output_dir())`。
    pub fn with_output_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.output_dir = Some(dir.as_ref().to_string_lossy().into_owned());
        self
    }
}

/// **优先使用的** shell —— 按可用性挑，而不是看 `COMSPEC`。
///
/// `COMSPEC` 答的是「系统默认命令解释器」（中文 Windows 上恒为 `cmd.exe`），
/// 但用户与工具链实际该用的是 PowerShell，照它写进 prompt 会误导模型。
/// 故按优先级探测：Windows `pwsh`(7+) → `powershell`(5.1) → `cmd`；其它平台 `$SHELL` → `sh`。
///
/// 只做 PATH 查找（`which`），**不 spawn**，所以能留在同步的 `detect_host()` 里。
fn preferred_shell() -> Option<String> {
    #[cfg(windows)]
    {
        ["pwsh", "powershell", "cmd"]
            .into_iter()
            .find(|name| which::which(name).is_ok())
            .map(str::to_string)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("SHELL")
            .and_then(|raw| {
                Path::new(&raw)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
            })
            .filter(|name| !name.trim().is_empty())
            .or_else(|| which::which("sh").is_ok().then(|| "sh".to_string()))
    }
}

/// Windows：禁止为子进程新建控制台窗口。
///
/// GUI 进程本身没有控制台，此时 spawn 一个 console 程序会让系统为它拉起
/// `conhost.exe` —— 表现就是「启动时冒出一堆黑框」，而且这个开销会把探测压满超时。
#[cfg(windows)]
fn no_window(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    command.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
fn no_window(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    command
}

/// 某个命令的版本参数：**`go` 用 `version`**（`go --version` 会失败），其余用 `--version`。
fn version_args(name: &str) -> &'static [&'static str] {
    if name == "go" {
        &["version"]
    } else {
        &["--version"]
    }
}

/// 该路径是否是 Microsoft Store 的别名存根（位于 `WindowsApps` 目录）。
///
/// 执行这种存根会**弹出应用商店**（且版本探测必然失败），因此不 spawn、直接判不可用。
pub(crate) fn is_store_stub(path: &Path) -> bool {
    if !cfg!(windows) {
        return false;
    }
    path.components()
        .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("WindowsApps"))
}

/// 从版本命令的输出里挑出「版本首行」。
///
/// stdout 优先；stdout 为空时退回 stderr —— ⚠️ Windows 的 `python --version`
/// 把版本写进 **stderr**，不读它就会永远探不到 python。
fn pick_version_text(stdout: &str, stderr: &str) -> Option<String> {
    let raw = if stdout.trim().is_empty() { stderr } else { stdout };
    let line = raw.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(truncate_chars(line, VERSION_MAX_CHARS))
}

/// 按字符数截断（避免切断多字节字符）。
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// 单个命令的探测结论。
enum ProbeOutcome {
    /// 可用；版本可能读不到（`None`）。
    Available(Option<String>),
    /// 不可用（PATH 没有 / Store 存根 / 跑不起来 / 超时）。
    Missing,
}

/// 并发探测一组命令的可用性。
pub async fn probe_executables(names: &[&str]) -> ExecutableProbe {
    let results = futures::future::join_all(names.iter().map(|name| probe_one(name))).await;

    let mut probe = ExecutableProbe::default();
    for (name, outcome) in names.iter().zip(results) {
        match outcome {
            ProbeOutcome::Available(version) => probe.available.push((name.to_string(), version)),
            ProbeOutcome::Missing => probe.missing.push(name.to_string()),
        }
    }
    probe
}

/// 探测单个命令：`which` 预过滤 Store 存根 → spawn 版本命令 → 定可用性。
///
/// 「可用」以**能否真的执行版本命令**为准（不是「PATH 里有没有」）。
async fn probe_one(name: &str) -> ProbeOutcome {
    // 1. PATH 解析不到 → 不可用
    let Ok(path) = which::which(name) else {
        return ProbeOutcome::Missing;
    };
    // 2. Store 存根：不 spawn（会弹应用商店）
    if is_store_stub(&path) {
        return ProbeOutcome::Missing;
    }
    // 3. 执行版本命令（带超时；直接跑解析出的路径，避免二次 PATH 查找）
    let mut command = tokio::process::Command::new(&path);
    command.args(version_args(name));
    let output = match tokio::time::timeout(PROBE_TIMEOUT, no_window(&mut command).output()).await {
        Ok(Ok(output)) => output,
        // 超时 / 启动失败 → 不可用
        _ => return ProbeOutcome::Missing,
    };
    if !output.status.success() {
        return ProbeOutcome::Missing;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    ProbeOutcome::Available(pick_version_text(&stdout, &stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_args_use_go_version() {
        assert_eq!(version_args("go"), &["version"], "go 必须用 `go version`");
        assert_eq!(version_args("node"), &["--version"]);
        assert_eq!(version_args("cargo"), &["--version"]);
    }

    #[test]
    fn store_stub_matches_windowsapps_path() {
        let stub = Path::new(r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\python.exe");
        let real = Path::new(r"C:\Program Files\nodejs\node.exe");
        // 只在 Windows 上启用该过滤
        assert_eq!(is_store_stub(stub), cfg!(windows));
        assert!(!is_store_stub(real));
    }

    #[test]
    fn pick_version_prefers_stdout_then_stderr() {
        assert_eq!(pick_version_text("v24.15.0\n", ""), Some("v24.15.0".to_string()));
        // Windows 的 python --version 把版本写 stderr
        assert_eq!(
            pick_version_text("", "Python 3.12.1\r\n"),
            Some("Python 3.12.1".to_string())
        );
        // 多行取首个非空行
        assert_eq!(
            pick_version_text("\nPHP 8.2.1\nCopyright (c)", ""),
            Some("PHP 8.2.1".to_string())
        );
        // 全空
        assert_eq!(pick_version_text("  \n", "\n"), None);
    }

    #[test]
    fn truncate_keeps_chars_intact() {
        assert_eq!(truncate_chars("abc", 5), "abc");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
    }

    #[test]
    fn detect_host_reports_platform() {
        let env = RuntimeEnvironment::detect_host();
        assert_eq!(env.os, std::env::consts::OS);
        assert_eq!(env.arch, std::env::consts::ARCH);
        assert_eq!(env.path_separator, std::path::MAIN_SEPARATOR);
        assert!(env.line_ending == "CRLF" || env.line_ending == "LF");
        // detect_host 不 spawn：可执行环境保持空
        assert!(env.executables.is_empty());
        assert!(env.probed_at.is_none());
    }

    /// shell 探测只做 PATH 查找（不 spawn），且任何受支持平台都该有一个默认 shell。
    #[test]
    fn preferred_shell_is_resolved_without_spawning() {
        let shell = preferred_shell().expect("任何受支持平台上都应能解析出一个 shell");
        assert!(!shell.trim().is_empty());
    }

    /// Windows 上不该因为 `COMSPEC` 就答成 `cmd`：只要有 PowerShell 就该优先报它。
    #[test]
    fn preferred_shell_prefers_powershell_over_comspec_cmd() {
        if !cfg!(windows) {
            return;
        }
        let has_powershell = which::which("powershell").is_ok() || which::which("pwsh").is_ok();
        if has_powershell {
            let shell = preferred_shell().unwrap();
            assert_ne!(shell, "cmd", "有 PowerShell 时不该报 cmd（COMSPEC 是另一回事）");
        }
    }

    #[tokio::test]
    async fn probe_missing_command_is_not_available() {
        // 必然不存在的命令名 → 走 which 失败分支，不 panic、不 spawn
        let probe = probe_executables(&["definitely-not-a-real-command-xyz"]).await;
        assert!(probe.available.is_empty());
        assert_eq!(probe.missing, vec!["definitely-not-a-real-command-xyz".to_string()]);
    }

    /// 「探测到可用命令」这条路径此前没有任何测试（只有 missing 分支有）。
    ///
    /// `cargo` 必然在 PATH —— 这个测试本身就是 cargo 跑起来的，因此断言与环境无关。
    #[tokio::test]
    async fn probe_real_command_reports_available_with_version() {
        let probe = probe_executables(&["cargo"]).await;
        assert!(
            probe.available.iter().any(|(name, _)| name == "cargo"),
            "cargo 应当可用；实际探测结果: {probe:?}"
        );
    }
}
