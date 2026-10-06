//! 文件 / 目录工具族的**共享支撑件**。
//!
//! 只保留本工具族（`crates/tool-manager/src/builtin/filesystem/`）**实际使用**的东西：
//! 契约结果构造、`io::Error` → 错误码、路径回显、内容指纹、编码/BOM 探测、二进制探测、参数解析。
//!
//! 与上一轮 `file_tools.rs` 的关系：旧 `fs_support.rs` 里面向 `std::fs` 的那半截
//! （`atomic_write` / verbatim 路径处理 / 换行风格 / 行号渲染 / 参数钳制 / 字符串截断）
//! **已随旧 `file_tools.rs` 一并删除** —— 新实现改由 cap-std 句柄做路径与写入
//! （`io/write.rs::atomic_write_in`），行号与换行风格按上游语义重新实现
//! （`io/read.rs` / `io/edit.rs`）。
//!
//! 设计稿：`docs/planned-agent/filesystem-tools-rewrite.md`。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::Path;

use serde_json::{Value, json};

use planned_agent_core::mcp::types::ToolResult;

// ── 结果构造 ────────────────────────────────────────────────────────────────

/// 构造工具结果。与 `system_tools.rs` 里的同名函数同形。
pub(crate) fn tool_result(content: Value, is_error: bool) -> ToolResult {
    ToolResult {
        call_id: uuid::Uuid::new_v4().to_string(),
        content,
        is_error,
    }
}

/// 构造带**错误码**的失败结果。
///
/// 约定：可预期的失败一律走这里（`Ok` + `is_error: true`），**不用 `Err`** ——
/// 上层会把 `Err` 拍平成一个字符串，错误码与结构全部丢失。
pub(crate) fn failure(code: &str, message: impl Into<String>) -> ToolResult {
    tool_result(json!({ "error": code, "message": message.into() }), true)
}

// ── 错误码与路径回显 ────────────────────────────────────────────────────────

/// 把 `io::ErrorKind` 映射到契约错误码。
///
/// `IsADirectory` / `NotADirectory` / `StorageFull` / `ReadOnlyFilesystem` 来自已稳定的
/// `io_error_more`（Rust 1.83+），本仓库工具链为 1.95，可直接用。
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
/// `io::Error` 只说「系统找不到指定的路径。 (os error 3)」，**不说是哪个路径** ——
/// 传错路径的一方（典型是拼写打错）只能看到一串没有信息量的错误码，进而反复换写法试错，
/// 白烧大量重试轮次（回归测试见 `filesystem/tests.rs` 的 `read_text_file_reports_missing_file`）。
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
/// 只用于审计里「同一次写入的内容可对账」，**不用于安全用途**，也**不能**用于查重 ——
/// `DefaultHasher` 跨运行不稳定（`find_duplicate_files` 因此走 size 分组 + 逐字节比对）。
/// `content` 本身不进日志（可能含敏感信息），只记这个。
pub(crate) fn content_hash(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

// ── 编码探测 ────────────────────────────────────────────────────────────────

/// 支持的文本编码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
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
            if !bytes.len().is_multiple_of(2) {
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

/// 行号前缀的列宽（`{:>6} | `）—— **全族唯一来源**：
/// `read_text_file`、`read_file_lines`（`with_line_numbers`）、`grep_file` 必须一致，
/// 否则同一份文件在不同工具里行号对不齐。
pub(crate) const LINE_NUMBER_WIDTH: usize = 6;

/// 开头 8 KiB 含 NUL 字节即判二进制。
///
/// 调用前必须先走 [`sniff_encoding`]：带 BOM 的 UTF-16 文本天然到处是 NUL，
/// 会被这里的判定误杀，所以 UTF-16 要先被 BOM 认下来、跳过本判定。
pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(BINARY_SNIFF_BYTES).any(|&b| b == 0)
}

// ── 参数解析 ────────────────────────────────────────────────────────────────

/// 从 JSON 值取字符串数组（非字符串项忽略）。
pub(crate) fn string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// ── 服务层错误 ──────────────────────────────────────────────────────────────

/// 服务层错误：自带**契约错误码**。
///
/// `code()` 就是写进失败结果 `{error, message}` 里 `error` 的值 —— 与 [`failure`] 同源，
/// 这样服务层（`core.rs` / `io/`）可以在任意深度返回 `FsError`，由外壳统一落成
/// `Ok + is_error` 的契约 JSON。
///
/// 只保留**当前真正构造**的变体；其余错误码（`not_a_directory` / `disk_full` 等）由
/// [`io_error_code`] 从 `io::Error` 映射，或由工具层直接 `failure("code", …)` 给出。
#[derive(Debug)]
pub(crate) enum FsError {
    /// 路径不存在
    NotFound(String),
    /// 路径不在任何允许目录内（cap-std 沙箱边界）
    OutsideAllowed(String),
    /// 编辑：找不到匹配
    NoMatch(String),
    /// 编辑：匹配不唯一
    AmbiguousMatch(String),
    /// 参数非法
    InvalidArgs(String),
    /// 底层 IO 错误（错误码由 [`io_error_code`] 映射）
    Io(io::Error),
}

impl FsError {
    /// 契约错误码。
    pub(crate) fn code(&self) -> &'static str {
        match self {
            FsError::NotFound(_) => "file_not_found",
            FsError::OutsideAllowed(_) => "path_outside_allowed",
            FsError::NoMatch(_) => "no_match",
            FsError::AmbiguousMatch(_) => "ambiguous_match",
            FsError::InvalidArgs(_) => "invalid_arguments",
            FsError::Io(e) => io_error_code(e),
        }
    }

    /// 人类可读消息。
    pub(crate) fn message(&self) -> String {
        match self {
            FsError::Io(e) => e.to_string(),
            FsError::NotFound(m)
            | FsError::OutsideAllowed(m)
            | FsError::NoMatch(m)
            | FsError::AmbiguousMatch(m)
            | FsError::InvalidArgs(m) => m.clone(),
        }
    }

    /// 落成契约失败结果（`Ok + is_error: true`）。
    pub(crate) fn into_tool_result(self) -> ToolResult {
        failure(self.code(), self.message())
    }
}

impl From<io::Error> for FsError {
    fn from(error: io::Error) -> Self {
        FsError::Io(error)
    }
}

pub(crate) type FsResult<T> = Result<T, FsError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_encoding_handles_boms_and_rejects_unknown() {
        let utf8_bom = [BOM_UTF8, b"hi"].concat();
        assert_eq!(
            sniff_encoding(&utf8_bom, "auto"),
            Ok((TextEncoding::Utf8, 3))
        );
        assert_eq!(
            sniff_encoding(&[0xFF, 0xFE, 0x68, 0x00], "auto"),
            Ok((TextEncoding::Utf16Le, 2))
        );
        assert_eq!(
            sniff_encoding(&[0xFE, 0xFF, 0x00, 0x68], "auto"),
            Ok((TextEncoding::Utf16Be, 2))
        );
        assert_eq!(sniff_encoding(b"hi", "auto"), Ok((TextEncoding::Utf8, 0)));
        assert!(
            sniff_encoding(b"hi", "gbk").is_err(),
            "gbk 未纳入（零新依赖）"
        );
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
    fn content_hash_is_stable_and_discriminating() {
        assert_eq!(content_hash(b"abc"), content_hash(b"abc"));
        assert_ne!(content_hash(b"abc"), content_hash(b"abd"));
    }

    #[test]
    fn error_codes_are_wired_to_the_contract() {
        assert_eq!(FsError::NotFound(String::new()).code(), "file_not_found");
        assert_eq!(
            FsError::OutsideAllowed(String::new()).code(),
            "path_outside_allowed"
        );
        assert_eq!(FsError::NoMatch(String::new()).code(), "no_match");
        assert_eq!(
            FsError::AmbiguousMatch(String::new()).code(),
            "ambiguous_match"
        );
        assert_eq!(FsError::InvalidArgs(String::new()).code(), "invalid_arguments");
        assert_eq!(
            FsError::from(io::Error::from(io::ErrorKind::NotFound)).code(),
            "file_not_found"
        );

        let result = FsError::NoMatch("没找到".to_string()).into_tool_result();
        assert!(result.is_error);
        assert_eq!(result.content["error"], "no_match");
        assert_eq!(result.content["message"], "没找到");
    }
}
