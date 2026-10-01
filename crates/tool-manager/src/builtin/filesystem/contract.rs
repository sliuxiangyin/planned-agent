//! filesystem 工具族的**对外契约**：schema + description。
//!
//! 本仓库 **description 才是唯一真正生效的契约** —— `ToolValidator` 只校验 `required` 缺失与字段类型，
//! 不看 `default` / `minimum` / `maximum` / `enum`。默认值与上下限一律在实现里自己兜。
//!
//! 语义对齐 rust-mcp-filesystem（工具名 / 参数名 / 返回形态），文案为本仓中文风格。

use serde_json::{json, Value};

// ── builtin_read_text_file ──────────────────────────────────────────────────

pub(crate) const READ_TEXT_FILE_DESCRIPTION: &str = concat!(
    "读取文本文件的完整内容，以文本返回。\n",
    "\n",
    "调用规则：\n",
    "1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "   只能在允许目录（沙箱白名单）内访问，越界返回 path_outside_allowed。\n",
    "2. with_line_numbers 默认 false；为 true 时每行前缀「右对齐行号 + 空格 + | + 空格 + 行文本」，\n",
    "   行号从 1 开始。示例：     1 | fn main() {}\n",
    "3. 返回的是**全文**，不做分页；大文件请改用 builtin_read_file_lines 按行范围读取。\n",
    "4. 自动处理 BOM（UTF-8 / UTF-16LE / UTF-16BE）；无法解码返回 invalid_encoding，不会静默替换字符。\n",
    "5. 二进制文件（开头 8 KiB 内含 NUL 字节）拒绝读取，返回 binary_file。\n",
    "6. 单文件硬上限 64 MiB，超限返回 content_too_large。\n",
    "7. 失败返回错误码：file_not_found / permission_denied / path_outside_allowed / is_a_directory /\n",
    "   invalid_encoding / binary_file / content_too_large / invalid_arguments / internal_error。\n",
);

pub(crate) fn read_text_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "文件路径。建议传绝对路径；必须在允许目录内。"
            },
            "with_line_numbers": {
                "type": "boolean",
                "default": false,
                "description": "为 true 时每行加「右对齐行号 + | 」前缀（行号从 1 开始），便于精确定位代码行。"
            }
        },
        "required": ["path"]
    })
}

// ── builtin_read_file_lines ─────────────────────────────────────────────────

pub(crate) const READ_FILE_LINES_DESCRIPTION: &str = concat!(
    "按行范围读取文本文件，以文本返回。适合大文件分页。\n",
    "\n",
    "调用规则：\n",
    "1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. offset 是起始行号，**从 0 开始**（0-based）；limit 是本次最多读取的行数，省略则读到文件末尾。\n",
    "3. 返回选中的行，行间用 \\n 连接，**不带行号前缀**。\n",
    "4. 编码 / 二进制 / 上限规则同 builtin_read_text_file。\n",
    "5. 失败返回错误码：file_not_found / permission_denied / path_outside_allowed / is_a_directory /\n",
    "   invalid_encoding / binary_file / content_too_large / invalid_arguments / internal_error。\n",
);

pub(crate) fn read_file_lines_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "文件路径。建议传绝对路径；必须在允许目录内。"
            },
            "offset": {
                "type": "integer",
                "minimum": 0,
                "description": "起始行号，从 0 开始（0-based）。"
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "description": "本次最多读取的行数；省略则读到文件末尾。"
            }
        },
        "required": ["path", "offset"]
    })
}

// ── builtin_write_file ──────────────────────────────────────────────────────

pub(crate) const WRITE_FILE_DESCRIPTION: &str = concat!(
    "写入文本文件（覆盖或新建）。\n",
    "\n",
    "调用规则：\n",
    "1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "   只能在允许目录内写入，越界返回 path_outside_allowed。\n",
    "2. content 是完整文件内容（UTF-8），上限 10 MiB。\n",
    "3. **本工具是纯覆盖**：不追加、不保留原内容；文件不存在则新建。\n",
    "4. **不会自动创建父目录**：父目录不存在请先用 builtin_create_directory。\n",
    "5. 写入是原子的（同目录临时文件 + rename），失败不会留下半截文件。\n",
    "6. 只改文件的一处/几处内容时请用 builtin_edit_file，不要整份重写（容易静默丢内容）。\n",
    "7. 失败返回错误码：file_not_found / permission_denied / path_outside_allowed / is_a_directory /\n",
    "   not_a_directory / content_too_large / disk_full / invalid_arguments / internal_error。\n",
);

pub(crate) fn write_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "文件路径。建议传绝对路径；必须在允许目录内。"
            },
            "content": {
                "type": "string",
                "description": "完整文件内容（UTF-8），上限 10 MiB。"
            }
        },
        "required": ["path", "content"]
    })
}

// ── builtin_create_directory ────────────────────────────────────────────────

pub(crate) const CREATE_DIRECTORY_DESCRIPTION: &str = concat!(
    "创建目录（支持多级嵌套）。目录已存在时视为成功。\n",
    "\n",
    "调用规则：\n",
    "1. path 是要创建的目录路径；相对路径基于进程当前工作目录解析，建议传绝对路径。\n",
    "2. 缺失的中间目录会自动创建。\n",
    "3. 失败返回错误码：permission_denied / path_outside_allowed / not_a_directory /\n",
    "   invalid_arguments / internal_error。\n",
);

pub(crate) fn create_directory_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "要创建的目录路径。建议传绝对路径；必须在允许目录内。"
            }
        },
        "required": ["path"]
    })
}

// ── builtin_move_file ───────────────────────────────────────────────────────

pub(crate) const MOVE_FILE_DESCRIPTION: &str = concat!(
    "移动或重命名文件 / 目录。\n",
    "\n",
    "调用规则：\n",
    "1. source / destination 都是路径；相对路径基于进程当前工作目录解析，建议传绝对路径。\n",
    "   两者都必须在允许目录内，越界返回 path_outside_allowed。\n",
    "2. **目标已存在则失败**（返回 already_exists）—— 本工具不覆盖。\n",
    "3. 目标父目录必须已存在（如需请先 builtin_create_directory）。\n",
    "4. 失败返回错误码：file_not_found / already_exists / permission_denied / path_outside_allowed /\n",
    "   invalid_arguments / internal_error。\n",
);

pub(crate) fn move_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "source": {
                "type": "string",
                "description": "源路径（文件或目录）。必须在允许目录内。"
            },
            "destination": {
                "type": "string",
                "description": "目标路径。必须在允许目录内，且不得已存在。"
            }
        },
        "required": ["source", "destination"]
    })
}

// ── builtin_edit_file ───────────────────────────────────────────────────────

pub(crate) const EDIT_FILE_DESCRIPTION: &str = concat!(
    "对文本文件做行级编辑，返回 git 风格 diff。修改已有文件请优先用它。\n",
    "\n",
    "调用规则：\n",
    "1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. edits 是编辑操作数组，每项 {oldText, newText}，按顺序依次应用。\n",
    "3. 匹配规则：先按 oldText 与文件内容做**精确匹配**；找不到时退化到**按行匹配**\n",
    "   （忽略每行首尾空白差异，结果按被替换块首行的缩进对齐）。\n",
    "4. 默认要求匹配唯一：精确匹配出现多次返回 ambiguous_match；设 replaceAll=true 可替换全部。\n",
    "   找不到匹配返回 no_match。\n",
    "5. dryRun=true 时只返回 diff、不写盘。\n",
    "6. 返回内容是 ```diff 围栏包裹的 unified diff。\n",
    "7. 写入是原子的；原文件的行尾风格（LF / CRLF）会被保留。\n",
    "8. 失败返回错误码：no_match / ambiguous_match / file_not_found / permission_denied /\n",
    "   path_outside_allowed / invalid_arguments / internal_error。\n",
);

pub(crate) fn edit_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "文件路径。建议传绝对路径；必须在允许目录内。"
            },
            "edits": {
                "type": "array",
                "description": "编辑操作数组，按顺序依次应用。",
                "items": {
                    "type": "object",
                    "properties": {
                        "oldText": {
                            "type": "string",
                            "description": "要被替换的原文（需与文件内容一致）。"
                        },
                        "newText": {
                            "type": "string",
                            "description": "替换成的内容；空字符串表示删除。"
                        }
                    },
                    "required": ["oldText", "newText"]
                }
            },
            "dryRun": {
                "type": "boolean",
                "default": false,
                "description": "为 true 时只返回 diff、不写盘。"
            },
            "replaceAll": {
                "type": "boolean",
                "default": false,
                "description": "为 true 时替换所有匹配；默认 false，此时匹配必须唯一。"
            }
        },
        "required": ["path", "edits"]
    })
}

// ── builtin_list_directory ──────────────────────────────────────────────────

pub(crate) const LIST_DIRECTORY_DESCRIPTION: &str = concat!(
    "列出目录内容。结果以 [FILE] / [DIR] 前缀区分文件与目录。\n",
    "\n",
    "调用规则：\n",
    "1. path 是目录路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "   只能在允许目录内访问，越界返回 path_outside_allowed。\n",
    "2. 结果按文件名排序，末尾附「Total: N files, M directories」。\n",
    "3. 需要每个文件的大小时用 builtin_list_directory_with_sizes；需要递归结构用 builtin_directory_tree。\n",
    "4. 失败返回错误码：file_not_found / not_a_directory / permission_denied / path_outside_allowed /\n",
    "   invalid_arguments / internal_error。\n",
);

pub(crate) fn list_directory_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "目录路径。建议传绝对路径；必须在允许目录内。"
            }
        },
        "required": ["path"]
    })
}

// ── builtin_list_directory_with_sizes ───────────────────────────────────────

pub(crate) const LIST_DIRECTORY_WITH_SIZES_DESCRIPTION: &str = concat!(
    "列出目录内容并附带每个文件的大小。结果以 [FILE] / [DIR] 前缀区分，末尾附汇总。\n",
    "\n",
    "调用规则：\n",
    "1. path 是目录路径；相对路径基于进程当前工作目录解析，建议传绝对路径。\n",
    "2. 文件行：[FILE] <名称> <大小>；目录行：[DIR]  <名称>。\n",
    "3. 末尾给出文件数、目录数与文件总大小。\n",
    "4. 失败返回错误码同 builtin_list_directory。\n",
);

pub(crate) fn list_directory_with_sizes_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "目录路径。建议传绝对路径；必须在允许目录内。"
            }
        },
        "required": ["path"]
    })
}

// ── builtin_directory_tree ──────────────────────────────────────────────────

pub(crate) const DIRECTORY_TREE_DESCRIPTION: &str = concat!(
    "以 JSON 结构返回递归目录树。每项 {name, type, children?}，type 为 file / directory，\n",
    "目录恒有 children（可为空）。\n",
    "\n",
    "调用规则：\n",
    "1. path 是根目录路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. max_depth 限制递归深度（可选）；不传时使用内部上限（32 层）。\n",
    "3. 超出深度或条目上限时，返回 JSON 的顶层会带 warning 字段。\n",
    "4. 目录为空时返回 internal_error。\n",
    "5. 失败返回错误码：file_not_found / not_a_directory / permission_denied / path_outside_allowed /\n",
    "   invalid_arguments / internal_error。\n",
);

pub(crate) fn directory_tree_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "根目录路径。建议传绝对路径；必须在允许目录内。"
            },
            "max_depth": {
                "type": "integer",
                "minimum": 1,
                "description": "限制递归深度（可选）。"
            }
        },
        "required": ["path"]
    })
}

// ── builtin_get_file_info ───────────────────────────────────────────────────

pub(crate) const GET_FILE_INFO_DESCRIPTION: &str = concat!(
    "获取文件或目录的详细元信息（大小、创建/修改/访问时间、权限、类型），不读取内容。\n",
    "\n",
    "调用规则：\n",
    "1. path 是文件或目录路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "   只能在允许目录内访问，越界返回 path_outside_allowed。\n",
    "2. 返回多行「字段: 值」文本。\n",
    "3. 失败返回错误码：file_not_found / permission_denied / path_outside_allowed /\n",
    "   invalid_arguments / internal_error。\n",
);

pub(crate) fn get_file_info_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "文件或目录路径。建议传绝对路径；必须在允许目录内。"
            }
        },
        "required": ["path"]
    })
}

// ── builtin_search_files ────────────────────────────────────────────────────

pub(crate) const SEARCH_FILES_DESCRIPTION: &str = concat!(
    "按 glob 搜索文件名（大小写不敏感）。\n",
    "\n",
    "调用规则：\n",
    "1. path 是搜索根目录；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. pattern 是 glob，如 *.rs、**/*.txt；同时匹配文件名与相对路径，大小写不敏感。\n",
    "3. excludePatterns 是排除用 glob 数组（可选）；min_bytes / max_bytes 按文件大小过滤（可选）。\n",
    "4. 返回匹配文件的完整路径列表。\n",
    "5. 失败返回错误码：file_not_found / not_a_directory / permission_denied / path_outside_allowed /\n",
    "   invalid_arguments / internal_error。\n",
);

pub(crate) fn search_files_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "搜索根目录。建议传绝对路径；必须在允许目录内。"
            },
            "pattern": {
                "type": "string",
                "description": "glob，如 *.rs 或 **/*.txt（大小写不敏感）。"
            },
            "excludePatterns": {
                "type": "array",
                "items": { "type": "string" },
                "description": "排除用 glob 数组（可选）。"
            },
            "min_bytes": {
                "type": "integer",
                "minimum": 0,
                "description": "最小文件大小（字节，可选）。"
            },
            "max_bytes": {
                "type": "integer",
                "minimum": 0,
                "description": "最大文件大小（字节，可选）。"
            }
        },
        "required": ["path", "pattern"]
    })
}

// ── builtin_search_files_content ────────────────────────────────────────────

pub(crate) const SEARCH_FILES_CONTENT_DESCRIPTION: &str = concat!(
    "在文件内容中搜索文本或正则，返回「路径:行号:列号: 行内容」。\n",
    "\n",
    "调用规则：\n",
    "1. path 是搜索根目录；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. query 是要找的内容；is_regex=true 时按正则解释（默认 false：字面量、大小写不敏感）。\n",
    "3. pattern（可选）按 glob 过滤文件名；excludePatterns 排除；min_bytes / max_bytes 按大小过滤。\n",
    "4. 二进制文件会被跳过；行号与列号都从 1 开始。\n",
    "5. 失败返回错误码：regex_error / file_not_found / not_a_directory / permission_denied /\n",
    "   path_outside_allowed / invalid_arguments / internal_error。\n",
);

pub(crate) fn search_files_content_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "搜索根目录。建议传绝对路径；必须在允许目录内。"
            },
            "query": {
                "type": "string",
                "description": "要搜索的内容（字面量或正则）。"
            },
            "is_regex": {
                "type": "boolean",
                "default": false,
                "description": "为 true 时把 query 当正则表达式。"
            },
            "pattern": {
                "type": "string",
                "description": "文件名 glob 过滤（可选）。"
            },
            "excludePatterns": {
                "type": "array",
                "items": { "type": "string" },
                "description": "排除用 glob 数组（可选）。"
            },
            "min_bytes": {
                "type": "integer",
                "minimum": 0,
                "description": "最小文件大小（字节，可选）。"
            },
            "max_bytes": {
                "type": "integer",
                "minimum": 0,
                "description": "最大文件大小（字节，可选）。"
            }
        },
        "required": ["path", "query"]
    })
}

// ── builtin_calculate_directory_size ────────────────────────────────────────

pub(crate) const CALCULATE_DIRECTORY_SIZE_DESCRIPTION: &str = concat!(
    "统计目录（含所有子目录）的文件总大小与数量。\n",
    "\n",
    "调用规则：\n",
    "1. root_path 是目录路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. output_format 取 text（默认）或 json。\n",
    "3. 失败返回错误码：file_not_found / not_a_directory / permission_denied / path_outside_allowed /\n",
    "   invalid_arguments / internal_error。\n",
);

pub(crate) fn calculate_directory_size_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "root_path": {
                "type": "string",
                "description": "目录路径。建议传绝对路径；必须在允许目录内。"
            },
            "output_format": {
                "type": "string",
                "enum": ["text", "json"],
                "default": "text",
                "description": "输出格式。"
            }
        },
        "required": ["root_path"]
    })
}

// ── builtin_find_duplicate_files ────────────────────────────────────────────

pub(crate) const FIND_DUPLICATE_FILES_DESCRIPTION: &str = concat!(
    "查找内容完全相同的重复文件。\n",
    "\n",
    "调用规则：\n",
    "1. root_path 是搜索根目录；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. pattern（可选）按 glob 过滤文件名；exclude_patterns（可选）排除；\n",
    "   min_bytes / max_bytes（可选）按大小过滤。\n",
    "3. output_format 取 text（默认）或 json。\n",
    "4. 空文件不参与比较；单文件上限 64 MiB。\n",
    "5. 失败返回错误码：file_not_found / not_a_directory / permission_denied / path_outside_allowed /\n",
    "   invalid_arguments / internal_error。\n",
);

pub(crate) fn find_duplicate_files_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "root_path": {
                "type": "string",
                "description": "搜索根目录。建议传绝对路径；必须在允许目录内。"
            },
            "pattern": {
                "type": "string",
                "description": "文件名 glob 过滤（可选）。"
            },
            "exclude_patterns": {
                "type": "array",
                "items": { "type": "string" },
                "description": "排除用 glob 数组（可选）。"
            },
            "min_bytes": {
                "type": "integer",
                "minimum": 0,
                "description": "最小文件大小（字节，可选）。"
            },
            "max_bytes": {
                "type": "integer",
                "minimum": 0,
                "description": "最大文件大小（字节，可选）。"
            },
            "output_format": {
                "type": "string",
                "enum": ["text", "json"],
                "default": "text",
                "description": "输出格式。"
            }
        },
        "required": ["root_path"]
    })
}

// ── builtin_find_empty_directories ──────────────────────────────────────────

pub(crate) const FIND_EMPTY_DIRECTORIES_DESCRIPTION: &str = concat!(
    "查找空目录（目录下没有任何条目）。\n",
    "\n",
    "调用规则：\n",
    "1. path 是搜索根目录；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. exclude_patterns（可选）用 glob 排除目录。\n",
    "3. output_format 取 text（默认）或 json。\n",
    "4. 失败返回错误码：file_not_found / not_a_directory / permission_denied / path_outside_allowed /\n",
    "   invalid_arguments / internal_error。\n",
);

pub(crate) fn find_empty_directories_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "搜索根目录。建议传绝对路径；必须在允许目录内。"
            },
            "exclude_patterns": {
                "type": "array",
                "items": { "type": "string" },
                "description": "排除用 glob 数组（可选）。"
            },
            "output_format": {
                "type": "string",
                "enum": ["text", "json"],
                "default": "text",
                "description": "输出格式。"
            }
        },
        "required": ["path"]
    })
}

// ── builtin_head_file / builtin_tail_file ───────────────────────────────────

pub(crate) const HEAD_FILE_DESCRIPTION: &str = concat!(
    "快速预览文件开头若干行。\n",
    "\n",
    "调用规则：\n",
    "1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. lines 是要返回的行数（从上往下）。编码 / 二进制 / 上限规则同 builtin_read_text_file。\n",
    "3. 失败返回错误码：file_not_found / permission_denied / path_outside_allowed / is_a_directory /\n",
    "   invalid_encoding / binary_file / content_too_large / invalid_arguments / internal_error。\n",
);

pub(crate) fn head_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "文件路径。必须在允许目录内。" },
            "lines": { "type": "integer", "minimum": 0, "description": "要返回的行数。" }
        },
        "required": ["path", "lines"]
    })
}

pub(crate) const TAIL_FILE_DESCRIPTION: &str = concat!(
    "快速预览文件末尾若干行。\n",
    "\n",
    "调用规则：\n",
    "1. path 是文件路径；相对路径基于进程当前工作目录解析（不是工作区根），建议传绝对路径。\n",
    "2. lines 是要返回的行数（从下往上）。编码 / 二进制 / 上限规则同 builtin_read_text_file。\n",
    "3. 失败返回错误码同 builtin_head_file。\n",
);

pub(crate) fn tail_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "文件路径。必须在允许目录内。" },
            "lines": { "type": "integer", "minimum": 0, "description": "要返回的行数。" }
        },
        "required": ["path", "lines"]
    })
}

// ── builtin_read_multiple_text_files ────────────────────────────────────────

pub(crate) const READ_MULTIPLE_TEXT_FILES_DESCRIPTION: &str = concat!(
    "批量读取多个文本文件。\n",
    "\n",
    "调用规则：\n",
    "1. paths 是文件路径数组；每项都必须能解析到允许目录内。\n",
    "2. 每个文件以「=== <路径> ===」分隔；单个文件失败不中断整体，错误内联标注。\n",
    "3. 总输出上限 1 MiB，超出后不再读取后续文件。\n",
);

pub(crate) fn read_multiple_text_files_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "要读取的文件路径数组（都在允许目录内）。"
            }
        },
        "required": ["paths"]
    })
}

// ── builtin_read_media_file / builtin_read_multiple_media_files ─────────────

pub(crate) const READ_MEDIA_FILE_DESCRIPTION: &str = concat!(
    "读取图片 / 音频 / 视频等媒体文件，返回 Base64 与 MIME 类型。\n",
    "\n",
    "调用规则：\n",
    "1. path 是媒体文件路径；相对路径基于进程当前工作目录解析，建议传绝对路径。\n",
    "2. max_bytes 限制单文件大小（默认 5 MiB）。\n",
    "3. 返回 JSON：{path, mime_type, size_bytes, data_base64}。\n",
);

pub(crate) fn read_media_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "媒体文件路径。必须在允许目录内。" },
            "max_bytes": { "type": "integer", "minimum": 0, "description": "单文件大小上限（字节，可选）。" }
        },
        "required": ["path"]
    })
}

pub(crate) const READ_MULTIPLE_MEDIA_FILES_DESCRIPTION: &str = concat!(
    "批量读取多个媒体文件，返回 JSON 数组。\n",
    "\n",
    "调用规则：\n",
    "1. paths 是媒体文件路径数组。\n",
    "2. max_bytes 限制单文件大小（默认 5 MiB）；单项失败不影响整体，失败项带 error / message 字段。\n",
);

pub(crate) fn read_multiple_media_files_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "paths": {
                "type": "array",
                "items": { "type": "string" },
                "description": "媒体文件路径数组（都在允许目录内）。"
            },
            "max_bytes": { "type": "integer", "minimum": 0, "description": "单文件大小上限（字节，可选）。" }
        },
        "required": ["paths"]
    })
}

// ── builtin_zip_files / builtin_zip_directory / builtin_unzip_file ──────────

pub(crate) const ZIP_FILES_DESCRIPTION: &str = concat!(
    "把指定文件打包成一个 ZIP。\n",
    "\n",
    "调用规则：\n",
    "1. input_files 是要打包的文件路径数组；target_zip_file 是输出 ZIP 路径。都在允许目录内。\n",
    "2. 压缩方式为 Deflate；ZIP 内条目名用源文件名。\n",
    "3. 先在内存里打包、再原子落盘，失败不会留下半截 ZIP。\n",
    "4. 失败返回错误码：file_not_found / not_a_directory / permission_denied / path_outside_allowed /\n",
    "   archive_error / invalid_arguments / internal_error。\n",
);

pub(crate) fn zip_files_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "input_files": {
                "type": "array",
                "items": { "type": "string" },
                "description": "要打包的文件路径数组。"
            },
            "target_zip_file": { "type": "string", "description": "输出 ZIP 路径。" }
        },
        "required": ["input_files", "target_zip_file"]
    })
}

pub(crate) const ZIP_DIRECTORY_DESCRIPTION: &str = concat!(
    "把一个目录按 glob 打包成 ZIP。\n",
    "\n",
    "调用规则：\n",
    "1. input_directory 是要打包的目录；target_zip_file 是输出 ZIP 路径。都在允许目录内。\n",
    "2. pattern 是 glob 过滤（默认 **/*）；ZIP 内条目名是相对该目录的路径。\n",
    "3. 失败返回错误码同 builtin_zip_files。\n",
);

pub(crate) fn zip_directory_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "input_directory": { "type": "string", "description": "要打包的目录。" },
            "pattern": { "type": "string", "default": "**/*", "description": "glob 过滤（可选）。" },
            "target_zip_file": { "type": "string", "description": "输出 ZIP 路径。" }
        },
        "required": ["input_directory", "target_zip_file"]
    })
}

pub(crate) const UNZIP_FILE_DESCRIPTION: &str = concat!(
    "解压 ZIP 到目标目录。\n",
    "\n",
    "调用规则：\n",
    "1. zip_file 是 ZIP 路径；target_path 是解压目标目录。都在允许目录内。\n",
    "2. 目标目录不存在会自动创建；ZIP 内 `..` / 绝对路径条目会被拒绝（zip-slip 防护）。\n",
    "3. 失败返回错误码：file_not_found / permission_denied / path_outside_allowed /\n",
    "   archive_error / invalid_arguments / internal_error。\n",
);

pub(crate) fn unzip_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "zip_file": { "type": "string", "description": "ZIP 文件路径。" },
            "target_path": { "type": "string", "description": "解压目标目录。" }
        },
        "required": ["zip_file", "target_path"]
    })
}
