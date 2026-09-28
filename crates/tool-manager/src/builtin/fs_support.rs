//! 文件工具的**共享支撑件**。
//!
//! 设计依据：`docs/planned-agent/file-tools-redesign.md` §4。
//!
//! 这里收拢的是 `read_file` / `write_file` / `list_dir` / `edit_file` 四个工具共用的**底层事实**：
//! 错误码与路径回显、参数钳制、编码与 BOM 探测、二进制探测、换行风格、原子写入、行号渲染、内容指纹。
//!
//! **刻意不碰 `system_tools.rs`**：那里有一份等价的 `failure` / `clamped_u64`
//! （`system_tools.rs:333` / `:338`，均为文件内私有）。把它们抽成公共 helper 会把这次改动的 diff
//! 扩大到与目标无关的工具上（还要重跑那 30+ 个测试）。宁可重复这十几个小函数。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::Path;

use serde_json::{json, Value};

use planned_agent_core::mcp::types::ToolResult;

// ── 结果构造 ────────────────────────────────────────────────────────────────

/// 构造工具结果。与 `system_tools.rs:324` 的 `tool_result` 同形。
pub(crate) fn tool_result(content: Value, is_error: bool) -> ToolResult {
    ToolResult {
        call_id: uuid::Uuid::new_v4().to_string(),
        content,
        is_error,
    }
}

/// 构造带**错误码**的失败结果（错误码表见设计稿 §3.4）。
///
/// 约定：可预期的失败一律走这里（`Ok` + `is_error: true`），**不用 `Err`** ——
/// 上层会把 `Err` 拍平成一个字符串，错误码与结构全部丢失
/// （`system_tools.rs:290-292` 记录的同一约定）。
pub(crate) fn failure(code: &str, message: impl Into<String>) -> ToolResult {
    tool_result(json!({ "error": code, "message": message.into() }), true)
}

// ── 参数钳制 ────────────────────────────────────────────────────────────────

/// 取整数参数并按上下限钳制。
///
/// schema 的 `default` / `minimum` / `maximum` 在本仓库**运行时不生效**：
/// `ToolValidator::validate_arguments`（`crates/tool-manager/src/core/validator.rs:11-56`）
/// 只检查 `required` 是否缺失（缺失直接返回 `Err`）与字段类型（不符只 `warn!`），
/// `default` / `minimum` / `maximum` / `enum` / `additionalProperties` 一个都不看。
/// 所以默认值与上下限必须在这里自己兜。
pub(crate) fn clamped_u64(value: Option<&Value>, default: u64, min: u64, max: u64) -> u64 {
    value
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .clamp(min, max)
}

/// 取字符串参数并按白名单校验（`enum` 校验的替代 —— validator 不看 `enum`）。
///
/// 返回 `Err(allowed)` 让调用方拼出「未知取值 + 可选值」的指导性错误，
/// 而不是默默退化成默认值：模型拼错枚举值时，「看起来成功但语义不对」比报错危险得多。
pub(crate) fn enum_arg<'a>(
    arguments: &'a Value,
    name: &str,
    default: &'a str,
    allowed: &[&str],
) -> Result<&'a str, String> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(s)) if allowed.contains(&s.as_str()) => Ok(s.as_str()),
        Some(Value::String(s)) => Err(format!(
            "未知 {name}：「{s}」（可选 {}）",
            allowed.join(" / ")
        )),
        Some(other) => Err(format!("{name} 必须是字符串，收到：{other}")),
    }
}

// ── 错误码与路径回显 ────────────────────────────────────────────────────────

/// 把 `io::ErrorKind` 映射到契约错误码（设计稿 §3.4）。
///
/// `IsADirectory` / `NotADirectory` / `StorageFull` / `ReadOnlyFilesystem` 这些变体
/// 来自已稳定的 `io_error_more`（Rust 1.83+），本仓库工具链为 1.95，可直接用。
pub(crate) fn io_error_code(error: &io::Error) -> &'static str {
    use io::ErrorKind::*;
    match error.kind() {
        NotFound => "file_not_found",
        PermissionDenied | ReadOnlyFilesystem => "permission_denied",
        AlreadyExists => "already_exists",
        IsADirectory => "is_a_directory",
        NotADirectory => "not_a_directory",
        StorageFull => "disk_full",
        _ => "internal_error",
    }
}

/// 构造**回显路径**的失败（错误码，消息）。
///
/// `std::fs` 的 `io::Error` 只说「系统找不到指定的路径。 (os error 3)」，**不说是哪个路径** ——
/// 传错路径的一方（典型是拼写/用户名打错）只能看到一串没有信息量的错误码，进而反复换写法试错，
/// 白烧大量重试轮次（真实案例见 `file_tools.rs` 的 `missing_path_error_echoes_the_path` 测试）。
pub(crate) fn path_error(action: &str, path: &Path, error: &io::Error) -> (&'static str, String) {
    let hint = if error.kind() == io::ErrorKind::NotFound {
        "（该路径不存在，请核对拼写）"
    } else {
        ""
    };
    (
        io_error_code(error),
        format!("{action}「{}」失败：{error}{hint}", path.display()),
    )
}

// ── 内容指纹 ────────────────────────────────────────────────────────────────

/// 内容指纹（`DefaultHasher`，零新依赖）。
///
/// 只用于审计里「同一次写入的内容可对账」，**不用于安全用途** —— 所以不上 `sha2`。
/// `content` 本身不进日志（可能含敏感信息），只记这个。
pub(crate) fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// ── 路径规范化 ──────────────────────────────────────────────────────────────

/// 去掉 Windows 的 verbatim 前缀。
///
/// `std::fs::canonicalize` 在 Windows 上返回 verbatim 路径（`\\?\D:\a\b`），
/// 直接回显给模型/写进日志会多出一段奇怪的 `\\?\`，且与用户给的路径对不上。
/// UNC 形式要还原成 `\\server\share`。
pub(crate) fn strip_verbatim(path: String) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path
    }
}

/// 解析后的真实路径（解析符号链接 / `..`），供审计与展示。
///
/// **不做任何准入判断** —— 工作区隔离已拍板不做（设计稿 Q2 / `system-tools-redesign.md` C5），
/// 这里纯粹是「文件到底在哪」的信息。文件不存在时（如刚删除、或写入新文件前）返回 `None`。
pub(crate) fn resolved_path(path: &Path) -> Option<String> {
    path.canonicalize()
        .ok()
        .map(|p| strip_verbatim(p.to_string_lossy().into_owned()))
}

// ── 换行风格 ────────────────────────────────────────────────────────────────

/// 探测换行风格：`lf` / `crlf` / `mixed`。
///
/// 没有任何换行符时按 `lf` 报（无从判定，且 `lf` 是跨平台更通用的默认）。
pub(crate) fn detect_newline(text: &str) -> &'static str {
    let bytes = text.as_bytes();
    let mut crlf = false;
    let mut lone_lf = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            if i > 0 && bytes[i - 1] == b'\r' {
                crlf = true;
            } else {
                lone_lf = true;
            }
        }
    }
    match (crlf, lone_lf) {
        (true, false) => "crlf",
        (true, true) => "mixed",
        _ => "lf",
    }
}

/// 把 `\r\n` 与孤立的 `\r` 统一成 `\n`。
pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 先把内容归一为 `\n`，再按目标风格重排。
///
/// 先归一是必须的：若内容里已有 `\r\n` 而又按 `crlf` 重排，会得到 `\r\r\n`。
/// `style` 只接受 `lf` / `crlf`；`preserve` / `none` 由调用方分别处理
/// （`none` 要原样写入，不能经过这里）。
pub(crate) fn apply_newline_style(text: &str, style: &str) -> String {
    let normalized = normalize_newlines(text);
    match style {
        "crlf" => normalized.replace('\n', "\r\n"),
        _ => normalized,
    }
}

// ── 编码与 BOM ──────────────────────────────────────────────────────────────

/// 本工具支持解码的编码。`gbk` 等需要 `encoding_rs`，按 Q4 的零新依赖拍板不纳入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl TextEncoding {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TextEncoding::Utf8 => "utf-8",
            TextEncoding::Utf16Le => "utf-16le",
            TextEncoding::Utf16Be => "utf-16be",
        }
    }
}

const BOM_UTF8: &[u8] = &[0xEF, 0xBB, 0xBF];
const BOM_UTF16_LE: &[u8] = &[0xFF, 0xFE];
const BOM_UTF16_BE: &[u8] = &[0xFE, 0xFF];

/// 决定按哪种编码解码，并给出**需要剥掉**的 BOM 字节数。
///
/// `auto` 只做 BOM 判定 + 「无 BOM 则 UTF-8」：识别不了「无 BOM 的 UTF-16」，
/// 那种文件会被当成二进制（前 8 KiB 全是 NUL 交替）—— 这是刻意的取舍，
/// 猜错编码的代价（静默乱码）比拒绝读取大得多。
pub(crate) fn sniff_encoding(
    bytes: &[u8],
    requested: &str,
) -> Result<(TextEncoding, usize), String> {
    match requested {
        "utf-8" => Ok((
            TextEncoding::Utf8,
            if bytes.starts_with(BOM_UTF8) { 3 } else { 0 },
        )),
        "utf-16le" => Ok((
            TextEncoding::Utf16Le,
            if bytes.starts_with(BOM_UTF16_LE) { 2 } else { 0 },
        )),
        "utf-16be" => Ok((
            TextEncoding::Utf16Be,
            if bytes.starts_with(BOM_UTF16_BE) { 2 } else { 0 },
        )),
        "auto" => {
            if bytes.starts_with(BOM_UTF8) {
                Ok((TextEncoding::Utf8, 3))
            } else if bytes.starts_with(BOM_UTF16_LE) {
                Ok((TextEncoding::Utf16Le, 2))
            } else if bytes.starts_with(BOM_UTF16_BE) {
                Ok((TextEncoding::Utf16Be, 2))
            } else {
                Ok((TextEncoding::Utf8, 0))
            }
        }
        other => Err(format!(
            "不支持的 encoding：「{other}」（可选 auto / utf-8 / utf-16le / utf-16be）"
        )),
    }
}

/// 严格解码。**不回退 `from_utf8_lossy`** —— 静默替换字符比报错危险得多。
pub(crate) fn decode(bytes: &[u8], encoding: TextEncoding) -> Result<String, String> {
    match encoding {
        TextEncoding::Utf8 => std::str::from_utf8(bytes)
            .map(|s| s.to_string())
            .map_err(|e| {
                format!(
                    "不是合法的 UTF-8（首个非法字节在偏移 {}）",
                    e.valid_up_to()
                )
            }),
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            if bytes.len() % 2 != 0 {
                return Err(format!(
                    "UTF-16 数据长度为奇数（{} 字节），无法解码",
                    bytes.len()
                ));
            }
            let little = encoding == TextEncoding::Utf16Le;
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| {
                    if little {
                        u16::from_le_bytes([c[0], c[1]])
                    } else {
                        u16::from_be_bytes([c[0], c[1]])
                    }
                })
                .collect();
            let mut out = String::with_capacity(units.len());
            for unit in std::char::decode_utf16(units) {
                match unit {
                    Ok(ch) => out.push(ch),
                    Err(e) => {
                        return Err(format!(
                            "UTF-16 含非法代理对（位置 {}）",
                            e.unpaired_surrogate() as usize
                        ))
                    }
                }
            }
            Ok(out)
        }
    }
}

// ── 二进制探测 ──────────────────────────────────────────────────────────────

/// 二进制探测只看文件开头这么多字节。
pub(crate) const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// 开头 8 KiB 含 NUL 字节即判二进制。
///
/// 调用前必须先走 [`sniff_encoding`]：带 BOM 的 UTF-16 文本天然到处是 NUL，
/// 会被这里的判定误杀，所以 UTF-16 要先被 BOM 认下来、跳过本判定。
pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SNIFF_BYTES).any(|&b| b == 0)
}

// ── 原子写入 ────────────────────────────────────────────────────────────────

/// 原子写入：**同目录**临时文件 → `sync_all` → `rename` 覆盖。
///
/// - **同目录**是关键：跨文件系统的 `rename` 不是原子操作。
/// - `std::fs::rename` 在 Windows 上走 `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`，可以覆盖已存在文件；
///   但目标被其它进程占用（编辑器 / 杀软 / 索引器）时会失败 —— 调用方必须把这种失败**报出去**
///   （不能静默失败，也不能退回非原子的直写）。
/// - **保留已有文件的权限**：`File::create` 建出的临时文件权限是「默认」（Unix 上是 `0o666 & ~umask`，
///   通常 0644），直接 rename 覆盖会把原文件的 `0600` / `0755` 静默放宽。Windows 的 ACL 不随 rename
///   变化，但**只读标志会变** —— 所以这一步在两端都有意义。
///   这里刻意用**跨平台 API**（`metadata().permissions()` / `set_permissions`）而不是 `#[cfg(unix)]`：
///   这样非 Unix 平台也会完整编译这段代码（cfg 掉的代码连语法错都发现不了）。
/// - 任何一步失败都清掉临时文件，不留垃圾。
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp = dir.join(format!(".{file_name}.tmp.{}", uuid::Uuid::new_v4()));

    // 覆盖前先取目标文件的权限；不存在则为 None（新建文件走默认权限）。
    let existing_permissions = std::fs::metadata(path).ok().map(|meta| meta.permissions());

    let write_result = (|| -> io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.flush()?;

        // 顺序很重要：必须在 write_all **之后** —— Windows 上先把文件置为只读，
        // 后续写入就会失败。
        if let Some(permissions) = existing_permissions.clone() {
            file.set_permissions(permissions)?;
        }

        file.sync_all()?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

// ── 行号渲染 ────────────────────────────────────────────────────────────────

/// 行号列的宽度：跟随总行数位数，至少 4（让 `   1→` 与 `  12→` 对齐）。
pub(crate) fn line_number_width(total_lines: usize) -> usize {
    total_lines.to_string().len().max(4)
}

/// 渲染一行：右对齐行号 + `→` + 行文本。
///
/// 行号**与 `offset` 同基准（1-based）**，避免模型「第 N 行」心智出现 off-by-one
/// （设计稿 §0.4-① 修正了指南里 0-based offset 与 1-based `no` 自相矛盾的问题）。
pub(crate) fn render_numbered_line(no: usize, text: &str, width: usize) -> String {
    format!("{no:>width$}→{text}", width = width)
}

/// 按**字符边界**安全截断（不会把一个多字节字符切成两半）。
pub(crate) fn truncate_at_char_boundary(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_newline_classifies_styles() {
        assert_eq!(detect_newline("a\nb\n"), "lf");
        assert_eq!(detect_newline("a\r\nb\r\n"), "crlf");
        assert_eq!(detect_newline("a\r\nb\n"), "mixed");
        assert_eq!(detect_newline("no newline"), "lf");
        assert_eq!(detect_newline(""), "lf");
    }

    #[test]
    fn apply_newline_style_never_doubles_carriage_returns() {
        assert_eq!(apply_newline_style("a\nb", "crlf"), "a\r\nb");
        assert_eq!(apply_newline_style("a\r\nb", "crlf"), "a\r\nb");
        assert_eq!(apply_newline_style("a\r\nb", "lf"), "a\nb");
        assert_eq!(apply_newline_style("a\rb", "lf"), "a\nb");
    }

    #[test]
    fn sniff_encoding_handles_boms_and_rejects_unknown() {
        let utf8_bom = [BOM_UTF8, b"hi"].concat();
        assert_eq!(sniff_encoding(&utf8_bom, "auto"), Ok((TextEncoding::Utf8, 3)));
        assert_eq!(
            sniff_encoding(&[0xFF, 0xFE, 0x68, 0x00], "auto"),
            Ok((TextEncoding::Utf16Le, 2))
        );
        assert_eq!(
            sniff_encoding(&[0xFE, 0xFF, 0x00, 0x68], "auto"),
            Ok((TextEncoding::Utf16Be, 2))
        );
        assert_eq!(sniff_encoding(b"hi", "auto"), Ok((TextEncoding::Utf8, 0)));
        assert!(sniff_encoding(b"hi", "gbk").is_err(), "gbk 未纳入（Q4 零新依赖）");
    }

    #[test]
    fn decode_is_strict_and_handles_utf16() {
        assert_eq!(decode("hi".as_bytes(), TextEncoding::Utf8).unwrap(), "hi");
        assert!(
            decode(&[0xFF, 0xFE], TextEncoding::Utf8).is_err(),
            "非法 UTF-8 必须报错，不能 lossy 替换"
        );
        assert_eq!(
            decode(&[0x68, 0x00, 0x69, 0x00], TextEncoding::Utf16Le).unwrap(),
            "hi"
        );
        assert_eq!(
            decode(&[0x00, 0x68, 0x00, 0x69], TextEncoding::Utf16Be).unwrap(),
            "hi"
        );
        assert!(
            decode(&[0x68, 0x00, 0x69], TextEncoding::Utf16Le).is_err(),
            "奇数长度无法解码"
        );
    }

    #[test]
    fn decode_rejects_unpaired_surrogate() {
        // 单独的高位代理（D800）没有配对低位代理
        assert!(decode(&[0x00, 0xD8], TextEncoding::Utf16Le).is_err());
    }

    #[test]
    fn looks_binary_only_looks_at_the_head() {
        assert!(looks_binary(&[0x89, b'P', b'N', b'G', 0x00]));
        assert!(!looks_binary(b"plain text"));
        let mut long = vec![b'a'; BINARY_SNIFF_BYTES + 10];
        long.push(0);
        assert!(!looks_binary(&long), "NUL 在探测窗口之外，不算二进制");
    }

    #[test]
    fn strip_verbatim_handles_drive_and_unc() {
        assert_eq!(strip_verbatim(r"\\?\D:\a\b".to_string()), r"D:\a\b");
        assert_eq!(
            strip_verbatim(r"\\?\UNC\server\share\f".to_string()),
            r"\\server\share\f"
        );
        assert_eq!(strip_verbatim(r"D:\a\b".to_string()), r"D:\a\b");
    }

    #[test]
    fn content_hash_is_stable_and_discriminating() {
        assert_eq!(content_hash(b"abc"), content_hash(b"abc"));
        assert_ne!(content_hash(b"abc"), content_hash(b"abd"));
    }

    #[test]
    fn clamped_u64_applies_defaults_and_limits() {
        assert_eq!(clamped_u64(None, 2000, 1, 10_000), 2000);
        assert_eq!(clamped_u64(Some(&json!(0)), 2000, 1, 10_000), 1);
        assert_eq!(clamped_u64(Some(&json!(99_999)), 2000, 1, 10_000), 10_000);
        assert_eq!(clamped_u64(Some(&json!("x")), 2000, 1, 10_000), 2000);
    }

    #[test]
    fn enum_arg_rejects_unknown_values_instead_of_defaulting() {
        assert_eq!(enum_arg(&json!({}), "mode", "overwrite", &["overwrite", "append"]).unwrap(), "overwrite");
        assert_eq!(
            enum_arg(&json!({"mode": "append"}), "mode", "overwrite", &["overwrite", "append"]).unwrap(),
            "append"
        );
        let err = enum_arg(&json!({"mode": "追加"}), "mode", "overwrite", &["overwrite", "append"])
            .unwrap_err();
        assert!(err.contains("未知 mode"), "{err}");
        assert!(err.contains("overwrite / append"), "{err}");
    }

    #[test]
    fn numbered_lines_share_the_offset_basis() {
        assert_eq!(line_number_width(9), 4);
        assert_eq!(line_number_width(12_345), 5);
        assert_eq!(render_numbered_line(1, "fn main() {", 4), "   1→fn main() {");
        assert_eq!(render_numbered_line(340, "}", 4), " 340→}");
    }

    #[test]
    fn truncate_at_char_boundary_keeps_utf8_valid() {
        let text = "中文abc";
        let cut = truncate_at_char_boundary(text, 4);
        assert_eq!(cut, "中", "4 落在第二个汉字中间，应回退到字符边界");
        assert_eq!(truncate_at_char_boundary(text, 100), text);
    }

    /// 覆盖已有文件必须**保留权限位**：`File::create` 建出的临时文件是 0644，
    /// 不复制原权限就会把 `0600` 静默放宽成 `0644`。
    /// 这是 unix-only 的断言 —— Windows 的 ACL 不随 rename 变化。
    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        std::fs::write(&file, b"old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();

        atomic_write(&file, b"new").unwrap();

        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "覆盖后权限被放宽了：{mode:o}");
    }

    /// 新建文件不走「保留权限」分支（`existing_permissions` 为 None）→ 用 umask 默认值。
    #[cfg(unix)]
    #[test]
    fn atomic_write_creates_new_files_with_default_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fresh.txt");

        atomic_write(&file, b"x").unwrap();

        assert_eq!(std::fs::read(&file).unwrap(), b"x");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_ne!(mode & 0o200, 0, "新建文件不该是只读的");
    }

    /// 临时文件不残留：写入成功后目录里只剩目标文件。
    #[test]
    fn atomic_write_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("only.txt");

        atomic_write(&file, b"body").unwrap();

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["only.txt"], "不该留下临时文件：{names:?}");
    }
}
