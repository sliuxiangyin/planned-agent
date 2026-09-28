//! 内置文件工具：`read_file` / `write_file` / `edit_file` / `list_dir`。
//!
//! 设计依据：`docs/planned-agent/file-tools-redesign.md`。
//!
//! 四个工具共享的底层事实（错误码、参数钳制、编码与 BOM、二进制探测、换行风格、原子写入、
//! 行号渲染）都在 [`super::fs_support`]。本文件只负责三件事：
//!
//! 1. **契约**：`schema` + `description` —— 后者是本仓库**唯一真正生效**的契约
//!    （`ToolValidator` 不校验 `default` / `minimum` / `maximum` / `enum` / `additionalProperties`）。
//! 2. **参数解释**：把 `Value` 解释成动作，非法取值返回 `invalid_arguments`（而不是默默用默认值）。
//! 3. **组装返回**：把底层结果拼成契约 JSON，并写 `tool_audit` 审计。

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

use planned_agent_core::mcp::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{BuiltinToolProvider, ToolCategory, ToolExecutor};

use super::fs_support::{
    BINARY_SNIFF_BYTES, TextEncoding, apply_newline_style, atomic_write, clamped_u64, content_hash,
    decode, detect_newline, enum_arg, failure, line_number_width, looks_binary, path_error,
    render_numbered_line, resolved_path, sniff_encoding, tool_result, truncate_at_char_boundary,
};

// ── 契约常量 ────────────────────────────────────────────────────────────────
//
// schema 里的 default / minimum / maximum / enum 在本仓库**运行时不生效**：
// `ToolValidator::validate_arguments`（crates/tool-manager/src/core/validator.rs:11-56）只检查
// required 是否缺失（缺失直接返回 `Err`）与字段类型（不符只 warn）。所以下面的默认值与上下限
// 必须在实现里自己兜。设计依据：file-tools-redesign.md §1.2 / §4.6。

const DEFAULT_READ_OFFSET: u64 = 1;
const MIN_READ_OFFSET: u64 = 1;
const DEFAULT_READ_LIMIT: u64 = 2_000;
const MIN_READ_LIMIT: u64 = 1;
const MAX_READ_LIMIT: u64 = 10_000;
const DEFAULT_MAX_BYTES: u64 = 262_144;
const MIN_MAX_BYTES: u64 = 1_024;
const MAX_MAX_BYTES: u64 = 10 * 1_048_576;

/// 单次读取「全文件入内存」的硬上限。
///
/// 设计稿写的是「文件总量不限制、按行分块」，但实现要先读全文才能给出 `total_lines` 与行号。
/// 与其假装不限，不如给一条明确的防爆线：超过就报 `content_too_large`，让调用方换手段。
/// （这是实现期对设计稿的一处**收紧**，已记在交付说明里。）
const MAX_FILE_BYTES: u64 = 64 * 1_048_576;

/// 单次写入 `content` 的上限（与 `system_tools.rs` 的 `max_output_bytes` 上限一致）。
const MAX_WRITE_BYTES: u64 = 10 * 1_048_576;

/// 沿用现有换行风格时，最多读文件开头这么多字节来判定风格。
const NEWLINE_SNIFF_BYTES: usize = 8 * 1024;

const ENCODINGS: &[&str] = &["auto", "utf-8", "utf-16le", "utf-16be"];
const WRITE_MODES: &[&str] = &["overwrite", "append", "create_new"];
const NEWLINE_STYLES: &[&str] = &["preserve", "lf", "crlf", "none"];

// ── description 定稿 ────────────────────────────────────────────────────────
//
// 中文、静态、**不注入平台信息**（平台事实归执行期运行环境段）。
// 每条都比 schema 更重要：模型真正读到的是这里。

const READ_DESCRIPTION: &str = concat!(
    "读取文本文件，按行返回并带行号前缀。\n",
    "\n",
    "调用规则：\n",
    "1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "   path 指向目录时返回 is_a_directory，请改用 builtin_list_dir。\n",
    "2. 默认从第 1 行开始、最多读 2000 行。大文件用 offset + limit 分批读；\n",
    "   has_more 为 true 表示后面还有内容。\n",
    "3. 返回的 content 是带行号的呈现（每行前缀「行号→」，行号从 1 开始、与 offset 同基准），\n",
    "   行间用 \\n 连接；原文换行风格见 newline 字段（lf / crlf / mixed），content 内不保留原文换行。\n",
    "4. 本工具返回的是派生视图，不是原文：要修改文件请用 builtin_edit_file，\n",
    "   不要 read_file + write_file 往返（会把行号写进正文、把 CRLF 改成 LF）。\n",
    "5. 二进制文件（开头 8 KiB 内含 NUL 字节）拒绝读取，返回 binary_file。\n",
    "6. 非 UTF-8 文件按 encoding 处理：默认 auto（先看 BOM）；无法解码返回 invalid_encoding，\n",
    "   不会静默替换字符。需要 GBK 等其它编码请先用其它工具转码。\n",
    "7. 单次返回受 max_bytes 限制（默认 256 KiB）；超出时截断并置 truncated=true，\n",
    "   续读起点见 next_offset（它等于 offset 时说明单行就超限，请提高 max_bytes）。\n",
    "8. 失败返回错误码：file_not_found / permission_denied / is_a_directory / invalid_encoding /\n",
    "   binary_file / content_too_large / invalid_arguments / internal_error。\n",
);

const WRITE_DESCRIPTION: &str = concat!(
    "写入文本文件。overwrite / append 走原子写入（同目录临时文件 + rename），失败不会留下半截文件。\n",
    "\n",
    "调用规则：\n",
    "1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. mode 三选一，请按意图明确选择：\n",
    "   overwrite（默认）覆盖或创建；append 追加到末尾（不存在则创建）；\n",
    "   create_new 仅在文件不存在时创建，已存在返回 already_exists。\n",
    "3. content 按 UTF-8 写入，上限 10 MiB；超限返回 content_too_large，请分块写。\n",
    "4. 换行：preserve（默认）沿用现有文件风格（新文件、以及探测不到换行时按 LF）；\n",
    "   需要强制 LF / CRLF 请显式指定。\n",
    "5. create_parents 默认 true，缺失的父目录会自动创建。\n",
    "6. 只改文件的一处/几处内容时，请用 builtin_edit_file，不要用本工具整份重写\n",
    "   （整份重写要求你把整个文件内容完整重新生成，容易静默丢内容）。\n",
    "7. 失败返回错误码：already_exists / permission_denied / content_too_large / is_a_directory /\n",
    "   disk_full / invalid_arguments / internal_error。\n",
);

const EDIT_DESCRIPTION: &str = concat!(
    "在文本文件中精确替换一个片段。只改动匹配到的内容，文件其余字节原样保留 —— 修改已有文件请优先用它。\n",
    "\n",
    "调用规则：\n",
    "1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. old_string 必须与文件内容逐字节一致（含缩进、换行、空格）。\n",
    "   默认要求它在文件中唯一出现：找不到返回 no_match，出现多次返回 ambiguous_match\n",
    "   （此时请补足上下文让片段唯一，或显式设 replace_all=true）。\n",
    "3. new_string 为空字符串表示删除 old_string 这段内容。\n",
    "4. 锚点是内容而不是行号 —— 行号会因其它编辑漂移，内容不会。\n",
    "5. 只支持 UTF-8 文件；写入走原子路径，失败不留半截文件。\n",
    "6. 失败返回错误码：no_match / ambiguous_match / file_not_found / permission_denied /\n",
    "   is_a_directory / invalid_encoding / binary_file / invalid_arguments / internal_error。\n",
);

const LIST_DIR_DESCRIPTION: &str = concat!(
    "列出目录下的条目名称（不递归、不含文件详情）。\n",
    "\n",
    "调用规则：\n",
    "1. path 是目录路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. 返回 entries（排序后的条目名数组）与 count。\n",
    "3. path 不存在返回 file_not_found；不是目录返回 not_a_directory。\n",
    "4. 要读取文件内容请用 builtin_read_file。\n",
);

// ── 内置文件工具提供者 ──────────────────────────────────────────────────────

/// 内置文件工具提供者。
pub struct FileToolsProvider;

impl BuiltinToolProvider for FileToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![
            (
                Tool {
                    name: "builtin_read_file".to_string(),
                    description: READ_DESCRIPTION.to_string(),
                    input_schema: read_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_write_file".to_string(),
                    description: WRITE_DESCRIPTION.to_string(),
                    input_schema: write_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_edit_file".to_string(),
                    description: EDIT_DESCRIPTION.to_string(),
                    input_schema: edit_schema(),
                },
                vec![ToolCategory::File],
            ),
            (
                Tool {
                    name: "builtin_list_dir".to_string(),
                    description: LIST_DIR_DESCRIPTION.to_string(),
                    input_schema: list_dir_schema(),
                },
                vec![ToolCategory::File],
            ),
        ]
    }

    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(FileToolsExecutor)
    }
}

fn read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "文件路径。相对路径基于进程当前工作目录（不是工作区根）；建议传绝对路径。"
            },
            "offset": {
                "type": "integer",
                "minimum": 1,
                "default": 1,
                "description": "起始行号（从 1 开始）。大文件配合 limit 分批读取。"
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 10000,
                "default": 2000,
                "description": "本次最多读取的行数，默认 2000，上限 10000。"
            },
            "encoding": {
                "type": "string",
                "enum": ["auto", "utf-8", "utf-16le", "utf-16be"],
                "default": "auto",
                "description": "编码。auto 先看 BOM，再按 UTF-8 严格解码；解码失败返回 invalid_encoding，不会静默替换字符。"
            },
            "max_bytes": {
                "type": "integer",
                "minimum": 1024,
                "maximum": 10485760,
                "default": 262144,
                "description": "本次返回的最大字节数（默认 256 KiB，上限 10 MiB）；超出部分截断并置 truncated。"
            }
        },
        "required": ["path"]
    })
}

fn write_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "文件路径。相对路径基于进程当前工作目录；建议传绝对路径。"
            },
            "content": {
                "type": "string",
                "description": "写入内容（UTF-8 文本），上限 10 MiB。"
            },
            "mode": {
                "type": "string",
                "enum": ["overwrite", "append", "create_new"],
                "default": "overwrite",
                "description": "overwrite 覆盖或创建；append 追加到末尾（不存在则创建）；create_new 仅在文件不存在时创建，已存在则报 already_exists。"
            },
            "create_parents": {
                "type": "boolean",
                "default": true,
                "description": "父目录不存在时是否自动创建。"
            },
            "ensure_newline": {
                "type": "string",
                "enum": ["preserve", "lf", "crlf", "none"],
                "default": "preserve",
                "description": "换行风格。preserve 沿用现有文件风格（新文件用 LF）；lf / crlf 强制；none 原样写入。"
            }
        },
        "required": ["path", "content"]
    })
}

fn edit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "文件路径。相对路径基于进程当前工作目录；建议传绝对路径。"
            },
            "old_string": {
                "type": "string",
                "description": "要被替换的原文片段，必须与文件内容逐字节一致（含缩进与换行）。默认要求在文件中唯一出现。"
            },
            "new_string": {
                "type": "string",
                "description": "替换后的内容；空字符串表示删除 old_string。"
            },
            "replace_all": {
                "type": "boolean",
                "default": false,
                "description": "为 true 时替换所有匹配项；默认 false，此时 old_string 必须唯一，否则报 ambiguous_match。"
            }
        },
        "required": ["path", "old_string", "new_string"]
    })
}

fn list_dir_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "目录路径。相对路径基于进程当前工作目录；建议传绝对路径。"
            }
        },
        "required": ["path"]
    })
}

// ── 执行器 ──────────────────────────────────────────────────────────────────

/// 文件工具执行器。
struct FileToolsExecutor;

#[async_trait]
impl ToolExecutor for FileToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        // 约定：可预期的失败一律返回 `Ok(ToolResult { is_error: true, content: {error, message} })`，
        // **不用 `Err`** —— 上层（chat / flexible）会把 `Err` 拍平成一个字符串，错误码与结构
        // 全部丢失（与 system_tools.rs:290-292 同一约定）。`Err` 只留给「未知工具名」这种编程错误。
        //
        // 这里的文件 IO 是阻塞调用（与改动前一致）：四个工具都是短小的单文件操作，
        // 不值得为它们引入 spawn_blocking 的复杂度。
        Ok(match tool_name {
            "builtin_read_file" => read_file(&arguments),
            "builtin_write_file" => write_file(&arguments),
            "builtin_edit_file" => edit_file(&arguments),
            "builtin_list_dir" => list_dir(&arguments),
            _ => return Err(anyhow::anyhow!("Unknown tool: {}", tool_name)),
        })
    }

    fn name(&self) -> &str {
        "builtin_file_tools"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec![
            "builtin_read_file".to_string(),
            "builtin_write_file".to_string(),
            "builtin_edit_file".to_string(),
            "builtin_list_dir".to_string(),
        ]
    }
}

/// 取必填的非空字符串参数。
fn required_path<'a>(arguments: &'a Value) -> Option<&'a str> {
    arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

// ── builtin_read_file ───────────────────────────────────────────────────────

fn read_file(arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = required_path(arguments) else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let path = Path::new(path_str);

    let encoding_name = match enum_arg(arguments, "encoding", "auto", ENCODINGS) {
        Ok(value) => value,
        Err(message) => return failure("invalid_arguments", message),
    };
    let offset =
        clamped_u64(arguments.get("offset"), DEFAULT_READ_OFFSET, MIN_READ_OFFSET, u64::MAX) as usize;
    let limit = clamped_u64(arguments.get("limit"), DEFAULT_READ_LIMIT, MIN_READ_LIMIT, MAX_READ_LIMIT)
        as usize;
    let max_bytes = clamped_u64(
        arguments.get("max_bytes"),
        DEFAULT_MAX_BYTES,
        MIN_MAX_BYTES,
        MAX_MAX_BYTES,
    ) as usize;

    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            let (code, message) = path_error("读取文件", path, &error);
            return failure(code, message);
        }
    };
    if metadata.is_dir() {
        return failure(
            "is_a_directory",
            format!("「{path_str}」是目录，请改用 builtin_list_dir"),
        );
    }
    let size = metadata.len();
    if size > MAX_FILE_BYTES {
        return failure(
            "content_too_large",
            format!(
                "「{path_str}」大小 {size} 字节，超过单次读取上限 {MAX_FILE_BYTES} 字节；请改用其它手段处理大文件"
            ),
        );
    }

    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            let (code, message) = path_error("读取文件", path, &error);
            return failure(code, message);
        }
    };

    let (encoding, bom_len) = match sniff_encoding(&bytes, encoding_name) {
        Ok(value) => value,
        Err(message) => return failure("invalid_arguments", message),
    };
    let body = &bytes[bom_len..];

    // 二进制判定要在解码之前，但**只在按 UTF-8 解释时**做：带 BOM 的 UTF-16 文本处处是 NUL，
    // 会被误杀，所以 UTF-16 先被 BOM 认下来、跳过这一关。
    if encoding == TextEncoding::Utf8 && looks_binary(body) {
        return failure(
            "binary_file",
            format!(
                "「{path_str}」是二进制文件（开头 {BINARY_SNIFF_BYTES} 字节内含 NUL，大小 {size} 字节），本工具不读取二进制"
            ),
        );
    }

    let text = match decode(body, encoding) {
        Ok(text) => text,
        Err(reason) => {
            return failure(
                "invalid_encoding",
                format!("「{path_str}」{reason}；可用 encoding 参数显式指定编码，或先用其它工具转码"),
            )
        }
    };

    let total_lines = text.lines().count();
    let newline = detect_newline(&text);
    let width = line_number_width(total_lines);

    let mut content = String::new();
    let mut returned = 0usize;
    let mut truncated = false;
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if number < offset {
            continue;
        }
        if returned >= limit {
            break;
        }
        let rendered = render_numbered_line(number, line, width);
        let extra = if content.is_empty() {
            rendered.len()
        } else {
            rendered.len() + 1
        };
        if content.len() + extra > max_bytes {
            truncated = true;
            // 首行本身就超上限时不能返回空内容：模型无从下手，还容易陷入重试。
            // 这里给出按字符边界截断的残行，但**不计入 returned** —— 该行并未被完整给出，
            // has_more 保持 true，模型应提高 max_bytes 或换手段。
            if content.is_empty() {
                content.push_str(truncate_at_char_boundary(&rendered, max_bytes));
            }
            break;
        }
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&rendered);
        returned += 1;
    }

    // 已消费的行数 = offset 之前的行 + 本次完整返回的行。
    let consumed = (offset - 1) + returned;
    let has_more = consumed < total_lines;

    let resolved = resolved_path(path);
    let modified = metadata
        .modified()
        .ok()
        .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339());
    let duration_ms = started.elapsed().as_millis() as u64;

    tracing::info!(
        target: "tool_audit",
        tool = "builtin_read_file",
        path = %path_str,
        resolved_path = ?resolved,
        offset,
        limit,
        lines = returned,
        total_lines,
        bytes_read = content.len(),
        encoding = encoding.as_str(),
        newline,
        truncated,
        duration_ms,
        "文件读取完毕"
    );

    tool_result(
        json!({
            "path": path_str,
            "resolved_path": resolved,
            "content": content,
            "offset": offset,
            "limit": limit,
            "total_lines": total_lines,
            "has_more": has_more,
            "next_offset": consumed + 1,
            "encoding": encoding.as_str(),
            "newline": newline,
            "bytes_read": content.len(),
            "truncated": truncated,
            "size": size,
            "modified": modified,
        }),
        false,
    )
}

// ── builtin_write_file ──────────────────────────────────────────────────────

fn write_file(arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = required_path(arguments) else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let path = Path::new(path_str);

    let Some(content) = arguments.get("content").and_then(Value::as_str) else {
        return failure("invalid_arguments", "content 必须是字符串（允许空字符串）");
    };
    let mode = match enum_arg(arguments, "mode", "overwrite", WRITE_MODES) {
        Ok(value) => value,
        Err(message) => return failure("invalid_arguments", message),
    };
    let newline_style = match enum_arg(arguments, "ensure_newline", "preserve", NEWLINE_STYLES) {
        Ok(value) => value,
        Err(message) => return failure("invalid_arguments", message),
    };
    let create_parents = arguments
        .get("create_parents")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    if std::fs::metadata(path).map(|meta| meta.is_dir()).unwrap_or(false) {
        return failure("is_a_directory", format!("「{path_str}」是目录"));
    }

    let existed = path.is_file();

    // 换行策略：
    //   none     → 原样写入（不走任何重排）
    //   preserve → 沿用现有文件风格；新文件用 lf；mixed 无法用单一风格表达，也按原样写入
    //   lf/crlf  → 先归一为 \n 再按目标风格重排（避免 \r\r\n）
    let target_newline: Option<&str> = match newline_style {
        "none" => None,
        "preserve" => {
            let detected = if existed {
                read_existing_newline(path).unwrap_or("lf")
            } else {
                "lf"
            };
            if detected == "mixed" {
                None
            } else {
                Some(detected)
            }
        }
        other => Some(other),
    };
    let bytes: Vec<u8> = match target_newline {
        Some(style) => apply_newline_style(content, style).into_bytes(),
        None => content.as_bytes().to_vec(),
    };

    // 上限校验放在换行重排**之后**：ensure_newline=crlf 会把每个 \n 扩成 \r\n，
    // 实际写入量可能接近两倍，按 content 判会漏。也必须在创建父目录**之前** ——
    // 否则超限返回时已经产生了「建了目录却没写文件」的副作用。
    let bytes_len = bytes.len() as u64;
    if bytes_len > MAX_WRITE_BYTES {
        return failure(
            "content_too_large",
            format!(
                "写入内容 {bytes_len} 字节（含换行重排）超过上限 {MAX_WRITE_BYTES} 字节，请分块写"
            ),
        );
    }

    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        if !parent.exists() {
            if !create_parents {
                // 父目录缺失时写入必然失败，提前给出明确错误（而不是让它以后续的
                // file_not_found / 权限错误的形式出现，那样看不出真正原因）。
                return failure(
                    "file_not_found",
                    format!(
                        "父目录「{}」不存在，且 create_parents=false（写入不会成功，故未执行）",
                        parent.display()
                    ),
                );
            }
            if let Err(error) = std::fs::create_dir_all(parent) {
                let (code, message) = path_error("创建父目录", parent, &error);
                return failure(code, format!("{message}（path 的父目录）"));
            }
        }
    }

    // created / replaced 由分支直接给出：append 既不创建也不替换（它只是追加），
    // create_new 只可能是新建。
    let (created, replaced) = match mode {
        "overwrite" => {
            if let Err(error) = atomic_write(path, &bytes) {
                let (code, mut message) = path_error("写入文件", path, &error);
                if code == "permission_denied" {
                    message.push_str("（若文件正被其它程序占用，请先关闭后再试）");
                }
                return failure(code, message);
            }
            (!existed, existed)
        }
        "create_new" => {
            // create_new(true) 让「不存在才创建」这一步是原子的。
            match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
                        // create_new(true) 已经把文件建出来了：写入失败必须清掉，
                        // 否则留下半截文件，且下次同路径 create_new 会变成 already_exists。
                        // 先关句柄再删 —— Windows 上占用中的文件删不掉。
                        drop(file);
                        let _ = std::fs::remove_file(path);
                        let (code, message) = path_error("写入新文件", path, &error);
                        return failure(code, message);
                    }
                }
                Err(error) => {
                    let (code, message) = path_error("创建文件", path, &error);
                    return failure(code, message);
                }
            }
            (true, false)
        }
        _ => {
            // append：语义就是「打开 — 定位末尾 — 写」，不能走「读全量 + 原子重写」
            // （那会在大文件上爆炸）。追加本身的并发风险是追加语义的固有属性。
            match std::fs::OpenOptions::new().create(true).append(true).open(path) {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(&bytes) {
                        let (code, message) = path_error("追加写入", path, &error);
                        return failure(code, message);
                    }
                }
                Err(error) => {
                    let (code, message) = path_error("打开文件", path, &error);
                    return failure(code, message);
                }
            }
            (!existed, false)
        }
    };

    let resolved = resolved_path(path);
    let written_newline = detect_newline(&String::from_utf8_lossy(&bytes));
    let duration_ms = started.elapsed().as_millis() as u64;

    tracing::info!(
        target: "tool_audit",
        tool = "builtin_write_file",
        path = %path_str,
        resolved_path = ?resolved,
        mode,
        bytes_written = bytes.len(),
        created,
        replaced,
        newline = written_newline,
        // content 本身不进日志（可能含敏感信息），只记指纹。
        content_hash = %content_hash(&bytes),
        duration_ms,
        "文件写入完毕"
    );

    tool_result(
        json!({
            "path": path_str,
            "resolved_path": resolved,
            "mode": mode,
            "bytes_written": bytes.len(),
            "created": created,
            "replaced": replaced,
            "newline": written_newline,
        }),
        false,
    )
}

/// 只为「沿用现有换行风格」读文件开头（不读全文）。
///
/// 若前 8 KiB 内一个换行符都没有，会退回 `lf` —— 为一次风格判断去读整个大文件不值得。
fn read_existing_newline(path: &Path) -> Option<&'static str> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = [0u8; NEWLINE_SNIFF_BYTES];
    let read = file.read(&mut buffer).ok()?;
    let head = String::from_utf8_lossy(&buffer[..read]);
    Some(detect_newline(&head))
}

// ── builtin_edit_file ───────────────────────────────────────────────────────

fn edit_file(arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = required_path(arguments) else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let path = Path::new(path_str);

    let Some(old_string) = arguments.get("old_string").and_then(Value::as_str) else {
        return failure("invalid_arguments", "old_string 必须是字符串");
    };
    if old_string.is_empty() {
        return failure(
            "invalid_arguments",
            "old_string 不能为空字符串（新建或整份重写请用 builtin_write_file）",
        );
    }
    let Some(new_string) = arguments.get("new_string").and_then(Value::as_str) else {
        return failure(
            "invalid_arguments",
            "new_string 必须是字符串（删除片段请传空字符串）",
        );
    };
    let replace_all = arguments
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            return failure("is_a_directory", format!("「{path_str}」是目录"));
        }
        Ok(metadata) if metadata.len() > MAX_FILE_BYTES => {
            // read_file 有同样的兜底；edit_file 也是全量读入，不能没有。
            return failure(
                "content_too_large",
                format!(
                    "「{path_str}」大小 {} 字节，超过可编辑上限 {MAX_FILE_BYTES} 字节",
                    metadata.len()
                ),
            );
        }
        _ => {}
    }

    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            let (code, message) = path_error("读取文件", path, &error);
            return failure(code, message);
        }
    };

    // edit_file 只在 UTF-8 上工作：写回时保持原编码意味着要为 UTF-16 重新编码，
    // 收益不足以抵消复杂度（write_file 也一律 UTF-8）。UTF-16 文件请先转码。
    let (encoding, bom_len) = match sniff_encoding(&bytes, "auto") {
        Ok(value) => value,
        Err(message) => return failure("invalid_arguments", message),
    };
    if encoding != TextEncoding::Utf8 {
        return failure(
            "invalid_encoding",
            format!(
                "「{path_str}」是 {} 编码，edit_file 只支持 UTF-8；请先转码，或改用 builtin_write_file",
                encoding.as_str()
            ),
        );
    }
    let body = &bytes[bom_len..];
    if looks_binary(body) {
        return failure(
            "binary_file",
            format!("「{path_str}」是二进制文件，无法按文本编辑"),
        );
    }
    let text = match decode(body, encoding) {
        Ok(text) => text,
        Err(reason) => return failure("invalid_encoding", format!("「{path_str}」{reason}")),
    };

    let occurrences = text.matches(old_string).count();
    if occurrences == 0 {
        return failure(
            "no_match",
            format!(
                "在「{path_str}」中找不到 old_string，未做任何修改（请确认片段与文件内容逐字节一致，含缩进与换行）"
            ),
        );
    }
    if occurrences > 1 && !replace_all {
        return failure(
            "ambiguous_match",
            format!(
                "old_string 在「{path_str}」中出现 {occurrences} 次，未做任何修改；请补足上下文使其唯一，或设 replace_all=true"
            ),
        );
    }
    let (updated, replacements) = if replace_all {
        (text.replace(old_string, new_string), occurrences)
    } else {
        (text.replacen(old_string, new_string, 1), 1)
    };

    // 保留原有 BOM（若有）：它是文件的一部分，不该被这次编辑悄悄抹掉。
    let mut output = Vec::with_capacity(updated.len() + bom_len);
    if bom_len > 0 {
        output.extend_from_slice(&bytes[..bom_len]);
    }
    output.extend_from_slice(updated.as_bytes());

    if let Err(error) = atomic_write(path, &output) {
        let (code, mut message) = path_error("写入文件", path, &error);
        if code == "permission_denied" {
            message.push_str("（若文件正被其它程序占用，请先关闭后再试）");
        }
        return failure(code, message);
    }

    let resolved = resolved_path(path);
    let newline = detect_newline(&updated);
    let duration_ms = started.elapsed().as_millis() as u64;

    tracing::info!(
        target: "tool_audit",
        tool = "builtin_edit_file",
        path = %path_str,
        resolved_path = ?resolved,
        replacements,
        bytes_written = output.len(),
        newline,
        content_hash = %content_hash(&output),
        duration_ms,
        "文件编辑完毕"
    );

    tool_result(
        json!({
            "path": path_str,
            "resolved_path": resolved,
            "replacements": replacements,
            "bytes_written": output.len(),
            "newline": newline,
        }),
        false,
    )
}

// ── builtin_list_dir ────────────────────────────────────────────────────────

fn list_dir(arguments: &Value) -> ToolResult {
    let started = Instant::now();

    let Some(path_str) = required_path(arguments) else {
        return failure("invalid_arguments", "path 必须是非空字符串");
    };
    let path = Path::new(path_str);

    if let Ok(metadata) = std::fs::metadata(path) {
        if !metadata.is_dir() {
            return failure("not_a_directory", format!("「{path_str}」不是目录"));
        }
    }

    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            let (code, message) = path_error("列出目录", path, &error);
            return failure(code, message);
        }
    };

    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();

    let resolved = resolved_path(path);
    let duration_ms = started.elapsed().as_millis() as u64;

    tracing::info!(
        target: "tool_audit",
        tool = "builtin_list_dir",
        path = %path_str,
        resolved_path = ?resolved,
        count = names.len(),
        duration_ms,
        "目录列举完毕"
    );

    tool_result(
        json!({
            "path": path_str,
            "resolved_path": resolved,
            "entries": names,
            "count": names.len(),
        }),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// 执行工具并断言**不返回 `Err`**：可预期的失败必须是 `Ok` + `is_error: true`。
    async fn exec(tool: &str, arguments: Value) -> ToolResult {
        FileToolsExecutor
            .execute(tool, arguments)
            .await
            .unwrap_or_else(|error| panic!("{tool} 不该返回 Err：{error}"))
    }

    fn error_code(result: &ToolResult) -> String {
        result.content["error"].as_str().unwrap_or_default().to_string()
    }

    fn text_content(result: &ToolResult) -> String {
        result.content["content"].as_str().unwrap_or_default().to_string()
    }

    fn path_of(path: &std::path::Path) -> String {
        path.to_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn unknown_tool_is_a_programming_error() {
        assert!(FileToolsExecutor.execute("nope", json!({})).await.is_err());
    }

    // ── read_file ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn read_file_numbers_lines_and_reports_metadata() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "fn main() {\n    hi\n}\n").unwrap();

        let result = exec("builtin_read_file", json!({ "path": path_of(&file) })).await;
        assert!(!result.is_error, "{:?}", result.content);
        assert_eq!(text_content(&result), "   1→fn main() {\n   2→    hi\n   3→}");
        assert_eq!(result.content["total_lines"], 3);
        assert_eq!(result.content["has_more"], false);
        assert_eq!(result.content["offset"], 1);
        assert_eq!(result.content["encoding"], "utf-8");
        assert_eq!(result.content["newline"], "lf");
        assert_eq!(result.content["truncated"], false);
        assert_eq!(result.content["size"].as_u64().unwrap(), 21);
        assert!(result.content["resolved_path"].is_string());
        assert!(result.content["modified"].is_string());
    }

    #[tokio::test]
    async fn read_file_pages_by_lines_and_reports_has_more() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("n.txt");
        let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        std::fs::write(&file, body).unwrap();

        let result = exec(
            "builtin_read_file",
            json!({ "path": path_of(&file), "offset": 4, "limit": 3 }),
        )
        .await;
        assert_eq!(result.content["total_lines"], 10);
        assert_eq!(result.content["offset"], 4);
        assert_eq!(result.content["has_more"], true);
        assert_eq!(
            text_content(&result),
            "   4→line4\n   5→line5\n   6→line6"
        );

        let tail = exec(
            "builtin_read_file",
            json!({ "path": path_of(&file), "offset": 10, "limit": 3 }),
        )
        .await;
        assert_eq!(text_content(&tail), "  10→line10");
        assert_eq!(tail.content["has_more"], false);
    }

    #[tokio::test]
    async fn read_file_beyond_end_is_empty_not_an_error() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("n.txt");
        std::fs::write(&file, "a\nb\n").unwrap();

        let result = exec(
            "builtin_read_file",
            json!({ "path": path_of(&file), "offset": 99 }),
        )
        .await;
        assert!(!result.is_error);
        assert_eq!(text_content(&result), "");
        assert_eq!(result.content["has_more"], false);
    }

    #[tokio::test]
    async fn read_file_handles_empty_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("empty.txt");
        std::fs::write(&file, "").unwrap();

        let result = exec("builtin_read_file", json!({ "path": path_of(&file) })).await;
        assert!(!result.is_error);
        assert_eq!(result.content["total_lines"], 0);
        assert_eq!(text_content(&result), "");
        assert_eq!(result.content["has_more"], false);
    }

    #[tokio::test]
    async fn read_file_reports_crlf_without_carriage_returns_in_content() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("crlf.txt");
        std::fs::write(&file, "a\r\nb\r\n").unwrap();

        let result = exec("builtin_read_file", json!({ "path": path_of(&file) })).await;
        assert_eq!(result.content["newline"], "crlf");
        assert_eq!(text_content(&result), "   1→a\n   2→b");
    }

    #[tokio::test]
    async fn read_file_rejects_binary() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("logo.png");
        std::fs::write(&file, [0x89u8, b'P', b'N', b'G', 0x00, 0x01, 0x02]).unwrap();

        let result = exec("builtin_read_file", json!({ "path": path_of(&file) })).await;
        assert!(result.is_error);
        assert_eq!(error_code(&result), "binary_file");
    }

    #[tokio::test]
    async fn read_file_rejects_invalid_utf8_without_lossy_replacement() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("bad.txt");
        std::fs::write(&file, [b'a', 0xFF, 0xFE, b'b']).unwrap();

        let result = exec("builtin_read_file", json!({ "path": path_of(&file) })).await;
        assert_eq!(error_code(&result), "invalid_encoding");
    }

    #[tokio::test]
    async fn read_file_decodes_utf16_by_bom() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("u16.txt");
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "hi\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&file, bytes).unwrap();

        let result = exec("builtin_read_file", json!({ "path": path_of(&file) })).await;
        assert!(!result.is_error, "{:?}", result.content);
        assert_eq!(result.content["encoding"], "utf-16le");
        assert_eq!(text_content(&result), "   1→hi");
    }

    #[tokio::test]
    async fn read_file_rejects_directory() {
        let dir = tempdir().unwrap();
        let result = exec("builtin_read_file", json!({ "path": path_of(dir.path()) })).await;
        assert_eq!(error_code(&result), "is_a_directory");
    }

    #[tokio::test]
    async fn read_file_truncates_at_max_bytes() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("big.txt");
        let body: String = (0..200).map(|_| format!("{}\n", "x".repeat(100))).collect();
        std::fs::write(&file, body).unwrap();

        let result = exec(
            "builtin_read_file",
            json!({ "path": path_of(&file), "max_bytes": 1024 }),
        )
        .await;
        assert_eq!(result.content["truncated"], true);
        assert_eq!(result.content["has_more"], true);
        assert!(text_content(&result).len() <= 1024);
        // 续读起点 = 已完整返回的行之后的那一行
        let returned_lines = text_content(&result).lines().count();
        assert_eq!(result.content["next_offset"], returned_lines + 1);
    }

    #[tokio::test]
    async fn read_file_truncates_an_oversized_first_line_without_returning_empty() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("oneline.txt");
        std::fs::write(&file, format!("{}\n", "y".repeat(5000))).unwrap();

        let result = exec(
            "builtin_read_file",
            json!({ "path": path_of(&file), "max_bytes": 1024 }),
        )
        .await;
        assert_eq!(result.content["truncated"], true);
        assert_eq!(result.content["has_more"], true);
        assert_eq!(
            result.content["next_offset"], 1,
            "该行未被完整给出，续读起点仍是第 1 行"
        );
        let content = text_content(&result);
        assert!(!content.is_empty(), "不能返回空内容让模型无从下手");
        assert!(content.len() <= 1024);
    }

    #[tokio::test]
    async fn read_file_reports_next_offset_for_paging() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("n.txt");
        let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        std::fs::write(&file, body).unwrap();

        let result = exec(
            "builtin_read_file",
            json!({ "path": path_of(&file), "offset": 4, "limit": 3 }),
        )
        .await;
        assert_eq!(result.content["next_offset"], 7);
    }

    #[tokio::test]
    async fn read_file_clamps_limit_in_implementation() {
        // schema 的 minimum 运行时不生效：limit=0 必须被实现钳到 1，而不是返回 0 行。
        let dir = tempdir().unwrap();
        let file = dir.path().join("n.txt");
        std::fs::write(&file, "a\nb\nc\n").unwrap();

        let result = exec(
            "builtin_read_file",
            json!({ "path": path_of(&file), "limit": 0 }),
        )
        .await;
        assert_eq!(result.content["limit"], 1);
        assert_eq!(text_content(&result), "   1→a");
    }

    #[tokio::test]
    async fn read_file_rejects_unknown_encoding_value() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();

        let result = exec(
            "builtin_read_file",
            json!({ "path": path_of(&file), "encoding": "gbk" }),
        )
        .await;
        assert_eq!(error_code(&result), "invalid_arguments");
    }

    /// 报错必须**回显路径**：否则调用方只看到一个没有信息量的错误码，无从得知是自己传的
    /// 路径写错了（真实案例：`C:\Users\wodpp\...` 比真实用户名多打了一个 p，白烧 10 轮重试）。
    #[tokio::test]
    async fn missing_path_error_echoes_the_path() {
        let missing = "C:/Users/no-such-user-wodpp/Desktop/Downloads";
        let result = exec("builtin_list_dir", json!({ "path": missing })).await;

        assert!(result.is_error);
        assert_eq!(error_code(&result), "file_not_found");
        let message = result.content["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(missing),
            "错误信息必须回显路径，否则无从定位：{message}"
        );
        assert!(
            message.contains("核对拼写"),
            "「路径不存在」应点明可能是拼写问题：{message}"
        );
    }

    // ── write_file ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn write_file_overwrite_creates_then_replaces() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("w.txt");

        let created = exec(
            "builtin_write_file",
            json!({ "path": path_of(&file), "content": "hello\n" }),
        )
        .await;
        assert!(!created.is_error, "{:?}", created.content);
        assert_eq!(created.content["created"], true);
        assert_eq!(created.content["replaced"], false);
        assert_eq!(created.content["bytes_written"], 6);
        assert_eq!(created.content["newline"], "lf");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello\n");

        let replaced = exec(
            "builtin_write_file",
            json!({ "path": path_of(&file), "content": "bye\n" }),
        )
        .await;
        assert_eq!(replaced.content["created"], false);
        assert_eq!(replaced.content["replaced"], true);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "bye\n");
    }

    #[tokio::test]
    async fn write_file_create_new_fails_when_present() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("w.txt");
        std::fs::write(&file, "keep\n").unwrap();

        let result = exec(
            "builtin_write_file",
            json!({ "path": path_of(&file), "content": "new\n", "mode": "create_new" }),
        )
        .await;
        assert_eq!(error_code(&result), "already_exists");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep\n", "不该动原文件");
    }

    #[tokio::test]
    async fn write_file_append_keeps_previous_content() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("log.txt");

        exec(
            "builtin_write_file",
            json!({ "path": path_of(&file), "content": "a\n", "mode": "append" }),
        )
        .await;
        let second = exec(
            "builtin_write_file",
            json!({ "path": path_of(&file), "content": "b\n", "mode": "append" }),
        )
        .await;
        assert!(!second.is_error, "{:?}", second.content);
        assert_eq!(second.content["created"], false);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "a\nb\n");
    }

    #[tokio::test]
    async fn write_file_create_parents_defaults_to_true() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("a/b/c.txt");

        let result = exec(
            "builtin_write_file",
            json!({ "path": path_of(&nested), "content": "x\n" }),
        )
        .await;
        assert!(!result.is_error, "{:?}", result.content);
        assert!(nested.exists());
    }

    #[tokio::test]
    async fn write_file_without_create_parents_reports_missing_parent() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("no/such/dir/f.txt");

        let result = exec(
            "builtin_write_file",
            json!({ "path": path_of(&nested), "content": "x", "create_parents": false }),
        )
        .await;
        assert_eq!(error_code(&result), "file_not_found");
        assert!(!nested.exists());
    }

    #[tokio::test]
    async fn write_file_preserves_existing_crlf() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("crlf.txt");
        std::fs::write(&file, "a\r\nb\r\n").unwrap();

        let result = exec(
            "builtin_write_file",
            json!({ "path": path_of(&file), "content": "x\ny\n" }),
        )
        .await;
        assert_eq!(result.content["newline"], "crlf");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "x\r\ny\r\n",
            "preserve 应沿用现有 CRLF"
        );
    }

    #[tokio::test]
    async fn write_file_can_force_newline_style() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("forced.txt");
        std::fs::write(&file, "a\r\nb\r\n").unwrap();

        exec(
            "builtin_write_file",
            json!({ "path": path_of(&file), "content": "x\ny\n", "ensure_newline": "lf" }),
        )
        .await;
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "x\ny\n");

        exec(
            "builtin_write_file",
            json!({ "path": path_of(&file), "content": "x\ny\n", "ensure_newline": "crlf" }),
        )
        .await;
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "x\r\ny\r\n");
    }

    #[tokio::test]
    async fn write_file_rejects_unknown_mode() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("w.txt");

        let result = exec(
            "builtin_write_file",
            json!({ "path": path_of(&file), "content": "x", "mode": "追加" }),
        )
        .await;
        assert_eq!(error_code(&result), "invalid_arguments");
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn write_file_rejects_content_over_the_limit() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("huge.txt");
        let huge = "x".repeat(MAX_WRITE_BYTES as usize + 1);

        let result = exec(
            "builtin_write_file",
            json!({ "path": path_of(&file), "content": huge }),
        )
        .await;
        assert_eq!(error_code(&result), "content_too_large");
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn write_file_rejects_directory_target() {
        let dir = tempdir().unwrap();
        let result = exec(
            "builtin_write_file",
            json!({ "path": path_of(dir.path()), "content": "x" }),
        )
        .await;
        assert_eq!(error_code(&result), "is_a_directory");
    }

    // ── edit_file ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn edit_file_replaces_a_unique_fragment_and_keeps_the_rest() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("e.rs");
        std::fs::write(&file, "let a = 1;\nlet b = 2;\n").unwrap();

        let result = exec(
            "builtin_edit_file",
            json!({
                "path": path_of(&file),
                "old_string": "let a = 1;",
                "new_string": "let a = 42;"
            }),
        )
        .await;
        assert!(!result.is_error, "{:?}", result.content);
        assert_eq!(result.content["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "let a = 42;\nlet b = 2;\n"
        );
    }

    #[tokio::test]
    async fn edit_file_reports_no_match_without_touching_the_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("e.txt");
        std::fs::write(&file, "original\n").unwrap();

        let result = exec(
            "builtin_edit_file",
            json!({ "path": path_of(&file), "old_string": "zzz", "new_string": "y" }),
        )
        .await;
        assert_eq!(error_code(&result), "no_match");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original\n");
    }

    #[tokio::test]
    async fn edit_file_reports_ambiguous_match_and_supports_replace_all() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("e.txt");
        std::fs::write(&file, "x\nx\n").unwrap();

        let ambiguous = exec(
            "builtin_edit_file",
            json!({ "path": path_of(&file), "old_string": "x", "new_string": "y" }),
        )
        .await;
        assert_eq!(error_code(&ambiguous), "ambiguous_match");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "x\nx\n");

        let all = exec(
            "builtin_edit_file",
            json!({
                "path": path_of(&file),
                "old_string": "x",
                "new_string": "y",
                "replace_all": true
            }),
        )
        .await;
        assert!(!all.is_error, "{:?}", all.content);
        assert_eq!(all.content["replacements"], 2);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "y\ny\n");
    }

    #[tokio::test]
    async fn edit_file_with_empty_new_string_deletes_the_fragment() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("e.txt");
        std::fs::write(&file, "keep\ndrop me\nkeep2\n").unwrap();

        let result = exec(
            "builtin_edit_file",
            json!({ "path": path_of(&file), "old_string": "drop me\n", "new_string": "" }),
        )
        .await;
        assert!(!result.is_error, "{:?}", result.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep\nkeep2\n");
    }

    #[tokio::test]
    async fn edit_file_keeps_crlf_and_bom() {
        let dir = tempdir().unwrap();

        let crlf = dir.path().join("crlf.txt");
        std::fs::write(&crlf, "a\r\nb\r\n").unwrap();
        exec(
            "builtin_edit_file",
            json!({ "path": path_of(&crlf), "old_string": "b", "new_string": "B" }),
        )
        .await;
        assert_eq!(std::fs::read_to_string(&crlf).unwrap(), "a\r\nB\r\n");

        let bom = dir.path().join("bom.txt");
        let mut bytes = vec![0xEFu8, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hi\n");
        std::fs::write(&bom, bytes).unwrap();
        exec(
            "builtin_edit_file",
            json!({ "path": path_of(&bom), "old_string": "hi", "new_string": "ho" }),
        )
        .await;
        assert_eq!(
            std::fs::read(&bom).unwrap(),
            [0xEF, 0xBB, 0xBF, b'h', b'o', b'\n'],
            "BOM 必须被保留"
        );
    }

    #[tokio::test]
    async fn edit_file_rejects_empty_old_string_and_missing_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("e.txt");
        std::fs::write(&file, "x\n").unwrap();

        let empty = exec(
            "builtin_edit_file",
            json!({ "path": path_of(&file), "old_string": "", "new_string": "y" }),
        )
        .await;
        assert_eq!(error_code(&empty), "invalid_arguments");

        let missing = exec(
            "builtin_edit_file",
            json!({ "path": path_of(&dir.path().join("nope.txt")), "old_string": "a", "new_string": "b" }),
        )
        .await;
        assert_eq!(error_code(&missing), "file_not_found");
    }

    // ── list_dir ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_dir_returns_sorted_entries() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();

        let result = exec("builtin_list_dir", json!({ "path": path_of(dir.path()) })).await;
        assert!(!result.is_error);
        assert_eq!(result.content["entries"], json!(["a.txt", "b.txt"]));
        assert_eq!(result.content["count"], 2);
    }

    #[tokio::test]
    async fn list_dir_on_a_file_is_not_a_directory() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "").unwrap();

        let result = exec("builtin_list_dir", json!({ "path": path_of(&file) })).await;
        assert_eq!(error_code(&result), "not_a_directory");
    }
}
