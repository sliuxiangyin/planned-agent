//! 往 system prompt 尾部追加「运行环境」段。
//!
//! 只陈述宿主事实（os / 路径分隔符 / 行尾 / shell / 可用命令 / 产出目录 / 编码风险 / 其它说明），
//! 用于消灭「环境不确定 → 反复试探」这类弯路。事实由 `core::host` 提供，这里只负责组织成文本。

use planned_agent_core::host::{RuntimeEnvironment, DEFAULT_PROBE_NAMES};

/// 把「运行环境段」写进 `out`（标题 + 宿主事实 + 编码风险）。
///
/// **不写** `working_dir` / `probed_at`：前者含用户名等路径信息（会随 prompt 发给
/// provider），后者与执行无关。
///
/// **唯一例外是 `output_dir`（产出目录）**：它同样含路径，但**刻意注入** —— 模型不知道
/// 产出落哪就会写到别处，而下游工具（如 `builtin_recognize_image` / `builtin_solve_captcha`）
/// 的沙箱根正是该目录，落错位置只能靠来回搬运。见设计稿 §5.2.5 的例外说明。
///
/// **不写**缺失命令清单：可用项本身就是完整信息 —— 没列出来的自然没有，
/// 把「探测范围」写进标题即可。
/// **不写**「怎么调用工具」：那属于工具契约，见 `builtin_execute_command` 的 description。
pub(super) fn render_environment_into(env: &RuntimeEnvironment, out: &mut String) {
    out.push_str("\n## 运行环境（事实，直接采用，不要试探确认）\n");

    // 产出目录**排在最前**：它是「写产出该落哪」的行动依据，比平台事实更直接决定下一步动作。
    if let Some(dir) = env.output_dir.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str("- 产出目录（工具的落盘产出都放这里）：");
        out.push_str(dir.trim());
        out.push('\n');
    }

    out.push_str("- 操作系统：");
    out.push_str(&env.os);
    out.push_str(" (");
    out.push_str(&env.arch);
    out.push_str(")\n");

    out.push_str("- 路径分隔符：");
    out.push(env.path_separator);
    out.push_str("，行尾：");
    out.push_str(&env.line_ending);
    out.push('\n');

    let shell = env.shell.as_deref().filter(|s| !s.is_empty());
    if let Some(shell) = shell {
        out.push_str("- 执行命令的 shell：");
        out.push_str(shell);
        out.push('\n');
    }

    if !env.executables.available.is_empty() {
        out.push_str("- 本机可用命令（已探测 ");
        out.push_str(&DEFAULT_PROBE_NAMES.join("/"));
        out.push_str("）：");
        out.push_str(&render_available(&env.executables.available));
        out.push('\n');
    }

    if let Some(hint) = shell.and_then(encoding_hint) {
        out.push_str(hint);
    }

    if let Some(notes) = env.notes.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str("- 其它说明：");
        out.push_str(notes.trim());
        out.push('\n');
    }
}

/// 渲染可用命令，如 `node v24.15.0、cargo 1.95.0`。
///
/// 版本原文常自带命令名前缀（`cargo --version` → `cargo 1.95.0 (…)`），直接拼会得到
/// `cargo cargo 1.95.0`，故剥掉与名字重复的前缀。
fn render_available(available: &[(String, Option<String>)]) -> String {
    available
        .iter()
        .map(|(name, version)| match version.as_deref().filter(|v| !v.is_empty()) {
            Some(version) => {
                let version = version
                    .strip_prefix(name.as_str())
                    .map_or(version, str::trim_start);
                format!("{name} {version}")
            }
            None => name.clone(),
        })
        .collect::<Vec<_>>()
        .join("、")
}

/// 编码风险一行 —— 只在**真的可能乱码**的 shell 下输出。
///
/// `pwsh`（7+）与 Unix shell 写 stdout 默认就是 UTF-8，不必提醒；
/// `powershell`（5.1）与 `cmd` 按**控制台代码页**写 stdout（中文系统为 GBK），
/// 而工具侧按 UTF-8 解码 → 中文会变成 `???`。
fn encoding_hint(shell: &str) -> Option<&'static str> {
    matches!(shell, "powershell" | "cmd").then_some(
        "- 命令输出按 UTF-8 解码：本机 shell 默认按控制台代码页输出（中文为 GBK），\
         中文可能变乱码 —— 需要时在那条命令里先切码（chcp 65001 / \
         [Console]::OutputEncoding=[Text.Encoding]::UTF8）。\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 产出目录：注入则渲染在**环境段最前**，未注入则整行省略。
    ///
    /// 与 `working_dir` 的对照很关键 —— 同为路径，一个刻意外发、一个刻意不外发。
    #[test]
    fn output_dir_line_only_when_injected() {
        let env = RuntimeEnvironment::detect_host();

        let mut out = String::new();
        render_environment_into(&env, &mut out);
        assert!(!out.contains("产出目录"), "未注入时不该出现该行: {out}");
        // 对照：`working_dir` 有值（`detect_host` 探的就是 cwd）却不渲染
        if let Some(dir) = env.working_dir.as_deref() {
            assert!(!out.contains(dir), "working_dir 不应进 prompt: {out}");
        }

        let env = env.with_output_dir(std::path::Path::new("D:\\cache"));
        let mut out = String::new();
        render_environment_into(&env, &mut out);
        assert!(
            out.contains("产出目录（工具的落盘产出都放这里）：D:\\cache"),
            "{out}"
        );
        // 排在平台事实之前：它是「写产出该落哪」的行动依据
        let dir_at = out.find("产出目录").expect("应渲染");
        let os_at = out.find("操作系统").expect("应渲染");
        assert!(dir_at < os_at, "产出目录应排在操作系统之前: {out}");
    }

    #[test]
    fn render_available_strips_duplicated_command_name_from_version() {
        // `cargo --version` 输出 `cargo 1.95.0 (…)`，直接拼会得到 `cargo cargo 1.95.0`
        let available = vec![
            (
                "cargo".to_string(),
                Some("cargo 1.95.0 (f2d3ce0bd 2026-03-21)".to_string()),
            ),
            (
                "rustc".to_string(),
                Some("rustc 1.95.0 (59807616e 2026-04-14)".to_string()),
            ),
            ("node".to_string(), Some("v24.15.0".to_string())),
            ("python".to_string(), None),
        ];
        let rendered = render_available(&available);
        assert!(rendered.contains("cargo 1.95.0"), "{rendered}");
        assert!(!rendered.contains("cargo cargo"), "{rendered}");
        assert!(!rendered.contains("rustc rustc"), "{rendered}");
        assert!(rendered.contains("node v24.15.0"), "{rendered}");
        assert!(rendered.ends_with("python"), "{rendered}");
    }

    #[test]
    fn encoding_hint_only_for_code_page_shells() {
        // pwsh（7+）与 Unix shell 写 stdout 默认 UTF-8，不必提醒
        assert!(encoding_hint("pwsh").is_none());
        assert!(encoding_hint("bash").is_none());
        // PowerShell 5.1 与 cmd 按控制台代码页写 stdout，中文会乱
        assert!(encoding_hint("powershell").is_some());
        assert!(encoding_hint("cmd").is_some());
    }
}
