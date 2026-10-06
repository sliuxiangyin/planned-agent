//! filesystem 工具族的行为测试。
//!
//! 覆盖：沙箱边界、读写语义（裸文本 / 0-based offset / 原子写）、编辑匹配规则、
//! 列举与树、搜索、统计、归档往返。

use std::path::Path;

use serde_json::{Value, json};
use tempfile::tempdir;

use planned_agent_core::mcp::types::ToolResult;
use planned_agent_core::tool_registry::BuiltinToolProvider;

use super::FilesystemProvider;

/// 以 `root` 为唯一沙箱根构造 provider。
fn provider(root: &Path) -> FilesystemProvider {
    FilesystemProvider::new(&[root.to_path_buf()]).expect("构造 provider 失败")
}

/// 执行工具并断言**不返回 `Err`**：可预期的失败必须是 `Ok` + `is_error: true`。
async fn exec(provider: &FilesystemProvider, tool: &str, arguments: Value) -> ToolResult {
    provider
        .executor()
        .execute(tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool} 不该返回 Err：{error}"))
}

/// 取纯文本结果（失败时回退到 JSON 打印，便于断言失败时看清内容）。
fn text(result: &ToolResult) -> String {
    match &result.content {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

/// 取错误码（成功时为 `None`）。
fn code(result: &ToolResult) -> Option<String> {
    result
        .content
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn p(path: &Path) -> String {
    path.to_str().expect("path 必须是有效 UTF-8").to_string()
}

// ── 沙箱 ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn resolve_rejects_paths_outside_allowed_root() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let outside_file = outside.path().join("secret.txt");
    std::fs::write(&outside_file, "secret").unwrap();

    let provider = provider(root.path());
    let result = exec(
        &provider,
        "builtin_read_text_file",
        json!({ "path": p(&outside_file) }),
    )
    .await;

    assert!(result.is_error);
    assert_eq!(code(&result).as_deref(), Some("path_outside_allowed"));
}

#[tokio::test]
async fn provider_without_roots_denies_everything() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "x").unwrap();

    let provider = FilesystemProvider::new(&[]).expect("空根也应能构造");
    let result = exec(
        &provider,
        "builtin_read_text_file",
        json!({ "path": p(&file) }),
    )
    .await;

    assert!(result.is_error);
    assert_eq!(code(&result).as_deref(), Some("path_outside_allowed"));
}

// ── 读取 ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn read_text_file_returns_bare_text_without_line_numbers() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "alpha\nbeta\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_read_text_file",
        json!({ "path": p(&file) }),
    )
    .await;

    assert!(!result.is_error);
    // ③ 裸文本：读全文原样返回（保留结尾换行），不做 trim。
    assert_eq!(text(&result), "alpha\nbeta\n");
}

#[tokio::test]
async fn read_text_file_with_line_numbers_uses_pipe_prefix() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "alpha\nbeta\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_read_text_file",
        json!({ "path": p(&file), "with_line_numbers": true }),
    )
    .await;

    let rendered = text(&result);
    assert!(rendered.contains("     1 | alpha"), "实际输出：{rendered}");
    assert!(rendered.contains("     2 | beta"), "实际输出：{rendered}");
}

#[tokio::test]
async fn read_file_lines_offset_is_zero_based() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_read_file_lines",
        json!({ "path": p(&file), "offset": 1, "limit": 1 }),
    )
    .await;

    // offset=1 表示「跳过第 1 行」，因此拿到第二行。
    assert_eq!(text(&result), "two");
}

#[tokio::test]
async fn read_file_lines_with_line_numbers_matches_text_file_format() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_read_file_lines",
        json!({ "path": p(&file), "offset": 1, "limit": 2, "with_line_numbers": true }),
    )
    .await;

    let rendered = text(&result);
    // 行号是**文件里的真实行号**（1-based），不是「本次选中的第几行」；
    // 格式与 builtin_read_text_file 一致（右对齐 + ` | `），见 contract.rs 的 LINE_NUMBER 说明。
    assert!(rendered.contains("     2 | two"), "实际输出：{rendered}");
    assert!(rendered.contains("     3 | three"), "实际输出：{rendered}");
    assert!(!rendered.contains("     1 | one"), "未选中的行不该出现：{rendered}");
}

#[tokio::test]
async fn read_file_lines_without_line_numbers_is_verbatim() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

    let provider = provider(dir.path());
    // 省略参数
    let omitted = exec(
        &provider,
        "builtin_read_file_lines",
        json!({ "path": p(&file), "offset": 1, "limit": 2 }),
    )
    .await;
    // 显式传 false
    let explicit = exec(
        &provider,
        "builtin_read_file_lines",
        json!({ "path": p(&file), "offset": 1, "limit": 2, "with_line_numbers": false }),
    )
    .await;

    // 零回归：与改动前逐字相同，且「省略」与「显式 false」两条路径无差别
    assert_eq!(text(&omitted), "two\nthree");
    assert_eq!(text(&explicit), "two\nthree");
}

#[tokio::test]
async fn read_text_file_rejects_binary_content() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("bin.dat");
    std::fs::write(&file, b"abc\0def").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_read_text_file",
        json!({ "path": p(&file) }),
    )
    .await;

    assert!(result.is_error);
    assert_eq!(code(&result).as_deref(), Some("binary_file"));
}

#[tokio::test]
async fn read_text_file_reports_missing_file() {
    let dir = tempdir().unwrap();
    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_read_text_file",
        json!({ "path": p(&dir.path().join("nope.txt")) }),
    )
    .await;

    assert!(result.is_error);
    assert_eq!(code(&result).as_deref(), Some("file_not_found"));
}

#[tokio::test]
async fn head_and_tail_file_slice_the_requested_edges() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "1\n2\n3\n4\n5\n").unwrap();

    let provider = provider(dir.path());
    let head = exec(&provider, "builtin_head_file", json!({ "path": p(&file), "lines": 2 })).await;
    assert_eq!(text(&head), "1\n2");

    let tail = exec(&provider, "builtin_tail_file", json!({ "path": p(&file), "lines": 2 })).await;
    assert_eq!(text(&tail), "4\n5");
}

#[tokio::test]
async fn read_multiple_text_files_concatenates_and_isolates_failures() {
    let dir = tempdir().unwrap();
    let first = dir.path().join("a.txt");
    let second = dir.path().join("b.txt");
    std::fs::write(&first, "AAA").unwrap();
    std::fs::write(&second, "BBB").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_read_multiple_text_files",
        json!({ "paths": [p(&first), p(&dir.path().join("missing.txt")), p(&second)] }),
    )
    .await;

    let output = text(&result);
    assert!(output.contains("AAA"));
    assert!(output.contains("BBB"));
    assert!(output.contains("file_not_found"), "缺失项应内联报错：{output}");
}

// ── 写入 ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn write_file_overwrites_and_leaves_no_temporary_file() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    let provider = provider(dir.path());

    let first = exec(
        &provider,
        "builtin_write_file",
        json!({ "path": p(&file), "content": "one" }),
    )
    .await;
    assert!(!first.is_error);

    let second = exec(
        &provider,
        "builtin_write_file",
        json!({ "path": p(&file), "content": "two" }),
    )
    .await;
    assert!(!second.is_error);

    // 纯覆盖：旧内容不残留
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "two");

    // 原子写的临时文件必须被清理
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty(), "残留临时文件：{leftovers:?}");
}

#[tokio::test]
async fn write_file_does_not_create_parent_directories() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("nested").join("a.txt");

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_write_file",
        json!({ "path": p(&file), "content": "x" }),
    )
    .await;

    assert!(result.is_error, "父目录不存在时必须失败（③ 纯覆盖语义）");
    assert!(!file.exists());
}

#[tokio::test]
async fn create_directory_makes_nested_directories() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("a").join("b").join("c");

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_create_directory",
        json!({ "path": p(&nested) }),
    )
    .await;

    assert!(!result.is_error);
    assert!(nested.is_dir());
}

#[tokio::test]
async fn move_file_refuses_an_existing_destination() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("src.txt");
    let destination = dir.path().join("dst.txt");
    std::fs::write(&source, "src").unwrap();
    std::fs::write(&destination, "dst").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_move_file",
        json!({ "source": p(&source), "destination": p(&destination) }),
    )
    .await;

    assert!(result.is_error);
    assert_eq!(code(&result).as_deref(), Some("already_exists"));
    // 双方都不得被改动
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "src");
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "dst");
}

#[tokio::test]
async fn move_file_renames_when_destination_is_free() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("src.txt");
    let destination = dir.path().join("dst.txt");
    std::fs::write(&source, "payload").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_move_file",
        json!({ "source": p(&source), "destination": p(&destination) }),
    )
    .await;

    assert!(!result.is_error);
    assert!(!source.exists());
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "payload");
}

// ── 编辑 ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn edit_file_applies_exact_match_and_writes_atomically() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "hello world\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_edit_file",
        json!({
            "path": p(&file),
            "edits": [{ "oldText": "world", "newText": "there" }]
        }),
    )
    .await;

    assert!(!result.is_error);
    assert!(text(&result).contains("```diff"), "应返回 diff 围栏");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello there\n");
}

#[tokio::test]
async fn edit_file_dry_run_does_not_touch_the_file() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "hello world\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_edit_file",
        json!({
            "path": p(&file),
            "edits": [{ "oldText": "world", "newText": "there" }],
            "dryRun": true
        }),
    )
    .await;

    assert!(!result.is_error);
    assert!(text(&result).contains("there"), "dryRun 仍要给出 diff");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world\n");
}

#[tokio::test]
async fn edit_file_line_level_match_tolerates_whitespace() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    // 文件里有缩进，传入的 oldText 没有 → 走行级匹配
    std::fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_edit_file",
        json!({
            "path": p(&file),
            "edits": [{ "oldText": "let x = 1;", "newText": "let x = 2;" }]
        }),
    )
    .await;

    assert!(!result.is_error);
    let updated = std::fs::read_to_string(&file).unwrap();
    // 缩进按被替换块首行对齐，必须保留 4 空格
    assert!(updated.contains("    let x = 2;"), "实际：{updated}");
}

#[tokio::test]
async fn edit_file_reports_no_match_and_ambiguous_match() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "dup\ndup\n").unwrap();
    let provider = provider(dir.path());

    let ambiguous = exec(
        &provider,
        "builtin_edit_file",
        json!({ "path": p(&file), "edits": [{ "oldText": "dup", "newText": "x" }] }),
    )
    .await;
    assert_eq!(code(&ambiguous).as_deref(), Some("ambiguous_match"));

    let missing = exec(
        &provider,
        "builtin_edit_file",
        json!({ "path": p(&file), "edits": [{ "oldText": "absent", "newText": "x" }] }),
    )
    .await;
    assert_eq!(code(&missing).as_deref(), Some("no_match"));

    // 失败不得改动文件
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "dup\ndup\n");
}

#[tokio::test]
async fn edit_file_replace_all_replaces_every_occurrence() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "dup\ndup\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_edit_file",
        json!({
            "path": p(&file),
            "edits": [{ "oldText": "dup", "newText": "x" }],
            "replaceAll": true
        }),
    )
    .await;

    assert!(!result.is_error);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "x\nx\n");
}

#[tokio::test]
async fn edit_file_preserves_crlf_line_endings() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "alpha\r\nbeta\r\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_edit_file",
        json!({ "path": p(&file), "edits": [{ "oldText": "beta", "newText": "gamma" }] }),
    )
    .await;

    assert!(!result.is_error);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha\r\ngamma\r\n");
}

// ── 列举 / 树 / 元信息 ──────────────────────────────────────────────────────

#[tokio::test]
async fn list_directory_marks_files_and_directories() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_list_directory",
        json!({ "path": p(dir.path()) }),
    )
    .await;

    let output = text(&result);
    assert!(output.contains("[DIR]  sub"), "实际：{output}");
    assert!(output.contains("[FILE] a.txt"), "实际：{output}");
    assert!(output.contains("Total: 1 files, 1 directories"), "实际：{output}");
}

#[tokio::test]
async fn list_directory_with_sizes_reports_totals() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "0123456789").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_list_directory_with_sizes",
        json!({ "path": p(dir.path()) }),
    )
    .await;

    let output = text(&result);
    assert!(output.contains("a.txt"), "实际：{output}");
    assert!(output.contains("Total size: 10 B"), "实际：{output}");
}

#[tokio::test]
async fn directory_tree_returns_nested_json() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("deep.txt"), "x").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_directory_tree",
        json!({ "path": p(dir.path()) }),
    )
    .await;

    assert!(!result.is_error);
    let payload: Value = serde_json::from_str(&text(&result)).expect("目录树必须是 JSON");
    let tree = payload["tree"].as_array().expect("tree 必须是数组");
    assert_eq!(tree[0]["name"], "sub");
    assert_eq!(tree[0]["type"], "directory");
    assert_eq!(tree[0]["children"][0]["name"], "deep.txt");
    assert_eq!(tree[0]["children"][0]["type"], "file");
}

#[tokio::test]
async fn directory_tree_respects_max_depth() {
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a").join("b")).unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_directory_tree",
        json!({ "path": p(dir.path()), "max_depth": 1 }),
    )
    .await;

    let payload: Value = serde_json::from_str(&text(&result)).unwrap();
    let a = &payload["tree"][0];
    assert_eq!(a["name"], "a");
    // 达深后不再下钻，children 为空
    assert_eq!(a["children"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn get_file_info_reports_size_and_type() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "12345").unwrap();

    let provider = provider(dir.path());
    let result = exec(&provider, "builtin_get_file_info", json!({ "path": p(&file) })).await;

    let output = text(&result);
    assert!(output.contains("size: 5"), "实际：{output}");
    assert!(output.contains("is_file: true"), "实际：{output}");
    assert!(output.contains("is_directory: false"), "实际：{output}");
}

// ── 搜索 / 统计 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn search_files_matches_glob_case_insensitively() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("ReadMe.MD"), "x").unwrap();
    std::fs::write(dir.path().join("other.txt"), "x").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_search_files",
        json!({ "path": p(dir.path()), "pattern": "readme.md" }),
    )
    .await;

    let output = text(&result);
    assert!(output.contains("ReadMe.MD"), "实际：{output}");
    assert!(!output.contains("other.txt"), "实际：{output}");
}

#[tokio::test]
async fn search_files_content_finds_literal_case_insensitively() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "Hello\nWorld\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_search_files_content",
        json!({ "path": p(dir.path()), "query": "world" }),
    )
    .await;

    let output = text(&result);
    assert!(output.contains("a.txt:2:"), "实际：{output}");
    assert!(output.contains("World"), "实际：{output}");
}

#[tokio::test]
async fn search_files_content_supports_regex() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "cat\ncot\ncut\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_search_files_content",
        json!({ "path": p(dir.path()), "query": "^c[ao]t$", "is_regex": true }),
    )
    .await;

    let output = text(&result);
    assert!(output.contains("cat"), "实际：{output}");
    assert!(output.contains("cot"), "实际：{output}");
    assert!(!output.contains("cut"), "实际：{output}");
}

#[tokio::test]
async fn search_files_content_reports_invalid_regex() {
    let dir = tempdir().unwrap();
    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_search_files_content",
        json!({ "path": p(dir.path()), "query": "([", "is_regex": true }),
    )
    .await;

    assert!(result.is_error);
    assert_eq!(code(&result).as_deref(), Some("regex_error"));
}

#[tokio::test]
async fn find_duplicate_files_groups_identical_content() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "same-content").unwrap();
    std::fs::write(dir.path().join("b.txt"), "same-content").unwrap();
    std::fs::write(dir.path().join("c.txt"), "unique-content").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_find_duplicate_files",
        json!({ "root_path": p(dir.path()) }),
    )
    .await;

    let output = text(&result);
    assert!(output.contains("a.txt"), "实际：{output}");
    assert!(output.contains("b.txt"), "实际：{output}");
    assert!(!output.contains("c.txt"), "实际：{output}");
}

#[tokio::test]
async fn find_empty_directories_reports_only_empty_ones() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("empty")).unwrap();
    std::fs::create_dir(dir.path().join("full")).unwrap();
    std::fs::write(dir.path().join("full").join("a.txt"), "x").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_find_empty_directories",
        json!({ "path": p(dir.path()) }),
    )
    .await;

    let output = text(&result);
    assert!(output.contains("empty"), "实际：{output}");
    assert!(!output.contains("full"), "实际：{output}");
}

#[tokio::test]
async fn calculate_directory_size_sums_nested_files() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub").join("a.txt"), "0123456789").unwrap();
    std::fs::write(dir.path().join("b.txt"), "01234").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_calculate_directory_size",
        json!({ "root_path": p(dir.path()), "output_format": "json" }),
    )
    .await;

    let payload: Value = serde_json::from_str(&text(&result)).expect("json 输出");
    assert_eq!(payload["total_size"], 15);
    assert_eq!(payload["file_count"], 2);
    assert_eq!(payload["directory_count"], 1);
}

// ── 媒体 / 归档 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn read_media_file_returns_base64_and_mime() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("dot.png");
    // PNG 魔数足够让 infer 识别
    std::fs::write(&file, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();

    let provider = provider(dir.path());
    let result = exec(&provider, "builtin_read_media_file", json!({ "path": p(&file) })).await;

    assert!(!result.is_error);
    assert_eq!(result.content["mime_type"], "image/png");
    assert_eq!(result.content["size_bytes"], 8);
    assert!(result.content["data_base64"].as_str().unwrap().starts_with("iVBOR"));
}

#[tokio::test]
async fn zip_then_unzip_roundtrips_content() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("a.txt");
    std::fs::write(&source, "zip-me").unwrap();
    let archive = dir.path().join("out.zip");
    let extracted = dir.path().join("extracted");

    let provider = provider(dir.path());

    let zipped = exec(
        &provider,
        "builtin_zip_files",
        json!({ "input_files": [p(&source)], "target_zip_file": p(&archive) }),
    )
    .await;
    assert!(!zipped.is_error, "打包失败：{:?}", code(&zipped));
    assert!(archive.is_file());

    let unzipped = exec(
        &provider,
        "builtin_unzip_file",
        json!({ "zip_file": p(&archive), "target_path": p(&extracted) }),
    )
    .await;
    assert!(!unzipped.is_error, "解压失败：{:?}", code(&unzipped));
    assert_eq!(
        std::fs::read_to_string(extracted.join("a.txt")).unwrap(),
        "zip-me"
    );
}

#[tokio::test]
async fn zip_directory_uses_relative_entry_names() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("pack")).unwrap();
    std::fs::write(dir.path().join("pack").join("inner.txt"), "inner").unwrap();
    let archive = dir.path().join("pack.zip");
    let extracted = dir.path().join("out");

    let provider = provider(dir.path());

    let zipped = exec(
        &provider,
        "builtin_zip_directory",
        json!({ "input_directory": p(&dir.path().join("pack")), "target_zip_file": p(&archive) }),
    )
    .await;
    assert!(!zipped.is_error, "打包目录失败：{:?}", code(&zipped));

    let unzipped = exec(
        &provider,
        "builtin_unzip_file",
        json!({ "zip_file": p(&archive), "target_path": p(&extracted) }),
    )
    .await;
    assert!(!unzipped.is_error);
    assert_eq!(
        std::fs::read_to_string(extracted.join("inner.txt")).unwrap(),
        "inner"
    );
}

#[tokio::test]
async fn unknown_tool_is_a_programming_error() {
    let dir = tempdir().unwrap();
    let provider = provider(dir.path());
    let error = provider
        .executor()
        .execute("builtin_nope", json!({}))
        .await
        .expect_err("未知工具应当返回 Err");
    assert!(error.to_string().contains("builtin_nope"));
}

#[tokio::test]
async fn every_registered_tool_is_dispatchable() {
    let dir = tempdir().unwrap();
    let provider = provider(dir.path());
    let names: Vec<String> = provider
        .tools()
        .into_iter()
        .map(|(tool, _)| tool.name)
        .collect();

    assert_eq!(names.len(), 24, "工具数量应与设计稿一致：{names:?}");

    // 每个名字都必须有分派分支（未知名字会返回 Err）
    let executor = provider.executor();
    for name in &names {
        let error = executor.execute(name, json!({})).await;
        if let Err(error) = error {
            panic!("{name} 没有分派分支：{error}");
        }
    }
}

// ── builtin_grep_file（单文件内容搜索） ─────────────────────────────────────

#[tokio::test]
async fn grep_file_finds_hits_with_context_and_line_numbers() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "alpha\nbeta\ngamma\ndelta\n").unwrap();

    let provider = provider(dir.path());
    let result = exec(
        &provider,
        "builtin_grep_file",
        json!({ "path": p(&file), "query": "gamma", "context_lines": 1 }),
    )
    .await;

    let rendered = text(&result);
    // 命中行以 `>` 标记、上下文行以空格标记，行号 1-based 且与 read_text_file 同列宽
    assert!(rendered.contains(">     3 | gamma"), "命中行：{rendered}");
    assert!(rendered.contains("\n      2 | beta"), "上文行：{rendered}");
    assert!(rendered.contains("\n      4 | delta"), "下文行：{rendered}");
    assert!(!rendered.contains("alpha"), "超出上下文的行不该出现：{rendered}");
    assert!(rendered.contains("共 1 处匹配"), "应给出命中总数：{rendered}");
    assert!(
        rendered.contains("offset = 行号 - 1"),
        "行号换算式必须写死在输出里：{rendered}"
    );
}

#[tokio::test]
async fn grep_file_paginates_with_match_offset() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "hit 1\nno\nhit 2\nno\nhit 3\n").unwrap();

    let provider = provider(dir.path());
    let first = exec(
        &provider,
        "builtin_grep_file",
        json!({ "path": p(&file), "query": "hit", "max_matches": 2, "context_lines": 0 }),
    )
    .await;
    let rendered = text(&first);
    assert!(
        rendered.contains("共 3 处匹配（本次返回第 1-2 处）；续读：match_offset=2"),
        "第一批应提示续读：{rendered}"
    );
    assert!(rendered.contains("     1 | hit 1"), "第一批首处：{rendered}");
    assert!(rendered.contains("     3 | hit 2"), "第一批次处：{rendered}");
    assert!(!rendered.contains("hit 3"), "第一批不该含第三处：{rendered}");

    let second = exec(
        &provider,
        "builtin_grep_file",
        json!({
            "path": p(&file), "query": "hit", "max_matches": 2,
            "match_offset": 2, "context_lines": 0
        }),
    )
    .await;
    let rendered = text(&second);
    assert!(rendered.contains("     5 | hit 3"), "第二批应含第三处：{rendered}");
    // 最后一批不再给续读提示，免得模型白跑一次
    assert!(
        rendered.contains("共 3 处匹配（本次返回第 3-3 处，已到最后）"),
        "最后一批不该提示续读：{rendered}"
    );
    assert!(!rendered.contains("match_offset=3"), "不该再提示续读：{rendered}");
}

#[tokio::test]
async fn grep_file_is_case_insensitive_by_default_and_sensitive_on_demand() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "Needle\nneedle\n").unwrap();

    let provider = provider(dir.path());
    let insensitive = exec(
        &provider,
        "builtin_grep_file",
        json!({ "path": p(&file), "query": "needle", "context_lines": 0 }),
    )
    .await;
    assert!(
        text(&insensitive).contains("共 2 处匹配"),
        "默认忽略大小写：{}",
        text(&insensitive)
    );

    let sensitive = exec(
        &provider,
        "builtin_grep_file",
        json!({ "path": p(&file), "query": "needle", "ignore_case": false, "context_lines": 0 }),
    )
    .await;
    assert!(
        text(&sensitive).contains("共 1 处匹配"),
        "显式 ignore_case=false 时区分大小写：{}",
        text(&sensitive)
    );
}

#[tokio::test]
async fn grep_file_marks_every_hit_even_when_contexts_overlap() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "hit a\nhit b\nhit c\n").unwrap();

    let provider = provider(dir.path());
    // context_lines=2 时三处命中的上下文完全重叠 —— 三行都必须是命中行（都带 `>`）。
    // 回归：旧实现用「行号 → 是否命中」的单个 map 边插边盖，后一个命中的上下文
    // 会把前一个命中行盖成非命中。
    let result = exec(
        &provider,
        "builtin_grep_file",
        json!({ "path": p(&file), "query": "hit", "context_lines": 2 }),
    )
    .await;

    let rendered = text(&result);
    assert!(rendered.contains(">     1 | hit a"), "第 1 行是命中：{rendered}");
    assert!(rendered.contains(">     2 | hit b"), "第 2 行是命中：{rendered}");
    assert!(rendered.contains(">     3 | hit c"), "第 3 行是命中：{rendered}");
    assert!(
        rendered.contains("共 3 处匹配（已全部返回）"),
        "三行各算一处：{rendered}"
    );
}

#[tokio::test]
async fn grep_file_rejects_directory_and_invalid_regex() {
    let dir = tempdir().unwrap();
    let provider = provider(dir.path());

    // 传目录：与 builtin_search_files_content 的报错方向正好相反
    let on_dir = exec(
        &provider,
        "builtin_grep_file",
        json!({ "path": p(dir.path()), "query": "x" }),
    )
    .await;
    assert!(on_dir.is_error, "传目录必须失败：{:?}", code(&on_dir));
    assert_eq!(code(&on_dir).as_deref(), Some("is_a_directory"));

    // 坏正则
    let bad_regex = exec(
        &provider,
        "builtin_grep_file",
        json!({ "path": p(dir.path()), "query": "(", "is_regex": true }),
    )
    .await;
    assert_eq!(code(&bad_regex).as_deref(), Some("regex_error"));
}
