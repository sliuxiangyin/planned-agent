//! `builtin_grep_file`：在**单个文件**里搜索内容，结果带行号与上下文，可续读。
//!
//! 与 `builtin_search_files_content` 的分工：那个在**目录**里搜（等价 `grep -r`，`path` 走
//! `open_dir`、**只接受目录**）；这个在**单文件**里搜 —— 落盘产出
//! （`docs/planned-agent/flexible-step-tool-output-spill.md`）是「一个文件一个句柄」，
//! 要的正是后者。设计见 `docs/planned-agent/builtin-grep-file.md`。
//!
//! 内部保留本仓横切强化（①）：cap-std 沙箱、BOM/编码探测、二进制拒读、单文件上限、
//! `tool_audit` 审计。

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use regex::Regex;
use serde_json::Value;

use planned_agent_core::mcp::types::ToolResult;

use crate::builtin::filesystem::core::FilesystemService;
use crate::builtin::filesystem::io::read::MAX_FILE_BYTES;
use crate::builtin::filesystem::support::{
    LINE_NUMBER_WIDTH, TextEncoding, decode, failure, looks_binary, path_error, sniff_encoding,
    tool_result,
};

use super::content::MAX_MATCHES;

/// 默认返回多少处命中（可用 `max_matches` 覆盖，硬上限 `MAX_MATCHES`）。
const DEFAULT_MAX_MATCHES: usize = 100;
/// 上下文行数：默认值与上限（每处命中前后各 N 行）。
const DEFAULT_CONTEXT_LINES: usize = 2;
const MAX_CONTEXT_LINES: usize = 10;

/// 单文件内容匹配器。
///
/// 与 `search/content.rs` 的 `Matcher` **刻意不复用**：那个要给出命中**列区间**（本工具不输出列号），
/// 且字面量**恒小写化**（大小写不敏感、没有开关）—— 本工具要支持大小写敏感，需求不同。
enum GrepMatcher {
    Literal { needle: String, ignore_case: bool },
    Regex(Regex),
}

impl GrepMatcher {
    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Literal {
                needle,
                ignore_case,
            } => {
                if *ignore_case {
                    line.to_lowercase().contains(needle.as_str())
                } else {
                    line.contains(needle.as_str())
                }
            }
            Self::Regex(regex) => regex.is_match(line),
        }
    }
}

/// `builtin_grep_file`
pub(crate) async fn grep_file(service: &FilesystemService, arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "path 必须是非空字符串（要搜索的**文件**）");
    };
    let Some(query) = arguments
        .get("query")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return failure("invalid_arguments", "query 必须是非空字符串（要搜索的内容）");
    };
    let is_regex = arguments
        .get("is_regex")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ignore_case = arguments
        .get("ignore_case")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let context_lines = arguments
        .get("context_lines")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_CONTEXT_LINES as u64)
        .min(MAX_CONTEXT_LINES as u64) as usize;
    let max_matches = arguments
        .get("max_matches")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_MATCHES as u64)
        .clamp(1, MAX_MATCHES as u64) as usize;
    let match_offset = arguments
        .get("match_offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    let matcher = if is_regex {
        match Regex::new(query) {
            Ok(regex) => GrepMatcher::Regex(regex),
            Err(error) => return failure("regex_error", format!("正则表达式无效：{error}")),
        }
    } else {
        GrepMatcher::Literal {
            needle: if ignore_case {
                query.to_lowercase()
            } else {
                query.to_string()
            },
            ignore_case,
        }
    };

    let path = Path::new(path_str);
    let resolved = match service.resolve(path).await {
        Ok(resolved) => resolved,
        Err(error) => return error.into_tool_result(),
    };

    // 先判目录：Windows 上「读目录」报 `PermissionDenied`、Unix 报 `IsADirectory`，
    // 只有显式判断才能给出**平台无关**且语义准确的 `is_a_directory`
    // （否则 Windows 用户看到的是 permission_denied，被误导成权限问题）。
    // 顺带指明该用哪个工具 —— 这是模型最容易走错的一步。
    if resolved
        .dir
        .metadata(&resolved.rel)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
    {
        return failure(
            "is_a_directory",
            format!(
                "「{path_str}」是目录；本工具只搜索**单个文件**，搜索整个目录请用 builtin_search_files_content"
            ),
        );
    }

    let bytes = match resolved.dir.read(&resolved.rel) {
        Ok(bytes) => bytes,
        Err(error) => {
            let (code, message) = path_error("读取文件", path, &error);
            return failure(code, message);
        }
    };
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return failure(
            "content_too_large",
            format!(
                "「{path_str}」{} 字节，超过单文件搜索上限 {MAX_FILE_BYTES} 字节",
                bytes.len()
            ),
        );
    }
    // 与 read_text_file / read_file_lines 走**同一组**底层判定（BOM + 编码探测 + 二进制保护），
    // 所以同一个文件「能读就能搜」，不会因编码判定不同而打架；
    // 只有失败文案按「搜索」措辞，故不复用 io::read 里那个写死「读取」的私有封装。
    let (encoding, bom_len) = match sniff_encoding(&bytes, "auto") {
        Ok(value) => value,
        Err(message) => return failure("invalid_arguments", message),
    };
    let body = &bytes[bom_len..];

    // 二进制判定只在按 UTF-8 解释时做：带 BOM 的 UTF-16 文本处处是 NUL，会被误杀。
    if encoding == TextEncoding::Utf8 && looks_binary(body) {
        return failure(
            "binary_file",
            format!("「{path_str}」是二进制文件（开头 8 KiB 内含 NUL），本工具不搜索二进制"),
        );
    }

    let text = match decode(body, encoding) {
        Ok(text) => text,
        Err(reason) => {
            return failure(
                "invalid_encoding",
                format!("「{path_str}」{reason}；本工具按 UTF-8 / BOM 解码，其它编码请先转码"),
            );
        }
    };

    let all_lines: Vec<&str> = text.lines().collect();
    let total_lines = all_lines.len();
    let hits: Vec<usize> = all_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| matcher.is_match(line))
        .map(|(index, _)| index + 1)
        .collect();
    let total_hits = hits.len();
    let display = resolved.display.join(&resolved.rel).display().to_string();

    if total_hits == 0 {
        let duration_ms = started.elapsed().as_millis() as u64;
        tracing::info!(
            target: "tool_audit",
            tool = "builtin_grep_file",
            path = %path_str,
            total_lines,
            total_hits,
            duration_ms,
            "单文件内容搜索：无命中"
        );
        return tool_result(
            Value::String(format!("没有匹配的内容。\n{display}：共 {total_lines} 行")),
            false,
        );
    }

    let page: Vec<usize> = hits
        .iter()
        .skip(match_offset)
        .take(max_matches)
        .copied()
        .collect();
    let returned = page.len();

    // match_offset 超界：明说，不能让模型误以为「没有匹配」。
    if page.is_empty() {
        return failure(
            "invalid_arguments",
            format!(
                "match_offset={match_offset} 已超出范围：{display} 共 {total_hits} 处匹配\
                 （有效范围 0-{}）；请用 0 从头开始或改小该值",
                total_hits - 1
            ),
        );
    }

    // 「要打印的行号」与「哪些行是命中」**分两个集合**：
    // 若用一个 map 边插边盖，后一个命中的上下文范围会把前一个命中行盖成非命中
    // （相邻命中时上下文完全重叠）—— 这正是旧写法的 bug。
    let hit_lines: BTreeSet<usize> = page.iter().copied().collect();
    let mut wanted: BTreeSet<usize> = BTreeSet::new();
    for &hit in &page {
        let start = hit.saturating_sub(context_lines).max(1);
        let end = (hit + context_lines).min(total_lines);
        wanted.extend(start..=end);
    }

    let mut output = String::new();
    let _ = writeln!(
        output,
        "{display}（共 {total_lines} 行；`>` 开头的是命中行，其余为上下文）"
    );
    for line_no in &wanted {
        let marker = if hit_lines.contains(line_no) { '>' } else { ' ' };
        let _ = writeln!(
            output,
            "{marker}{:>width$} | {}",
            line_no,
            all_lines[line_no - 1],
            width = LINE_NUMBER_WIDTH
        );
    }
    let next_offset = match_offset + returned;
    if next_offset < total_hits {
        let _ = writeln!(
            output,
            "共 {total_hits} 处匹配（本次返回第 {}-{} 处）；续读：match_offset={next_offset}",
            match_offset + 1,
            next_offset
        );
    } else if match_offset > 0 {
        // 已到最后一批：不再给续读提示，否则模型会白跑一次。
        let _ = writeln!(
            output,
            "共 {total_hits} 处匹配（本次返回第 {}-{} 处，已到最后）",
            match_offset + 1,
            next_offset
        );
    } else {
        let _ = writeln!(output, "共 {total_hits} 处匹配（已全部返回）");
    }
    // 行号基准写死在给模型的文本里，不指望它自己推这层换算（设计稿 §3.1）。
    let _ = write!(
        output,
        "行号从 1 开始；用 builtin_read_file_lines 精读时 offset = 行号 - 1（0-based），\
         可加 with_line_numbers=true 核对"
    );

    let duration_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        target: "tool_audit",
        tool = "builtin_grep_file",
        path = %path_str,
        total_lines,
        total_hits,
        returned,
        match_offset,
        context_lines,
        duration_ms,
        "单文件内容搜索完毕"
    );

    tool_result(Value::String(output), false)
}
