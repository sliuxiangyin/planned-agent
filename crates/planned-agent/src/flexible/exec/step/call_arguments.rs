//! 把「只有文件名」的路径类参数补成本次执行的产出目录。
//!
//! ## 它解决什么
//!
//! 模型调用工具时常给**裸文件名**（`browser_take_screenshot {"filename": "captcha-1.png"}`）。
//! 相对名的解析基准是 **MCP server 进程的 cwd**（= GUI 进程 cwd），于是产出落到仓库根，
//! 而不是本次执行的产出目录。prompt 纪律压不住它 —— 工具自己的 description
//! （"**File name** to save the screenshot to"）就在调用点，比 system prompt 更近。
//! 所以这里在**调用前**用代码兜：把裸名补成产出目录下的完整路径。
//!
//! ## 规则（刻意收窄）
//!
//! 1. **只处理裸文件名** —— 去掉一个 `./` 前缀后仍不含 `/` `\` `:` 的值。
//!    含任何路径成分（`sub/a.png`、`../a.png`、`D:\a.png`）一律**不碰**，
//!    因此天然没有「双重拼接」问题，也不需要路径归一化。
//! 2. **只看字段名** —— `input_schema.properties` 里名字像本地路径的参数
//!    （`filename` / `path` / `*_file` …）。schema 拿不到、或没有 `properties`
//!    （如 `additionalProperties: true`）→ **一律不动**（安全降级）。
//! 3. **不区分读/写** —— 裸名一律补产出目录。读类（上传文件、加载脚本）若因此
//!    找不到文件，由调用点的「失败回退」用原参数重试兜住（见 `mod.rs`）。
//! 4. **递归**处理数组元素与嵌套对象。
//!
//! ## 纪律
//!
//! - 回灌文案（[`render_rewrites`]）**不点名工具**：写死工具名在它改名 / 未注册时
//!   会变成幻觉源（与 `join_text_and_image_notes` 同一条纪律）。
//! - 只改**参数值**，不碰模型自己的消息记录 —— 否则模型会看到与它生成的不一致。
//! - **无共享状态**：产出目录由调用点传入，多个 run 并发推进时不会互相污染。

use std::path::Path;

use planned_agent_core::mcp::types::Tool;
use serde_json::Value;

/// 一次改写的记录（回灌给模型 + 审计用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRewrite {
    /// 参数位置：顶层是字段名；数组下标形如 `paths[0]`；嵌套形如 `output.path`
    pub key: String,
    /// 模型给的原值
    pub from: String,
    /// 改写后的完整路径
    pub to: String,
}

/// 判定 + 改写：返回（改写后的参数，改写记录）。
///
/// `output_dir` 应为**本次执行的产出目录**（`cfg.cache_dir` + `run_dir`）——
/// 与工具输出落盘、图片落盘同一个目录。
pub fn resolve(
    tool: Option<&Tool>,
    mut arguments: Value,
    output_dir: &Path,
) -> (Value, Vec<PathRewrite>) {
    let mut rewrites = Vec::new();

    // 产出目录为空 → `"".join("a.png")` 还是 `a.png`：补了等于没补，却会记一条
    // 「假改写」污染回灌文案与日志。宁可不做。
    if output_dir.as_os_str().is_empty() {
        tracing::debug!(
            target: "tool_path_normalization",
            "产出目录为空，跳过路径补全（补了等于没补）"
        );
        return (arguments, rewrites);
    }

    // schema 拿不到 → 不改（宁可不做，也不要瞎猜哪个参数是路径）。
    let Some(tool) = tool else {
        tracing::debug!(
            target: "tool_path_normalization",
            "工具定义未找到，跳过路径补全（拿不到 inputSchema，无从判定哪个参数是路径）"
        );
        return (arguments, rewrites);
    };
    let Some(properties) = tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
    else {
        tracing::debug!(
            target: "tool_path_normalization",
            tool = %tool.name,
            "inputSchema 没有 properties，跳过路径补全"
        );
        return (arguments, rewrites);
    };
    let Some(object) = arguments.as_object_mut() else {
        return (arguments, rewrites);
    };

    // 被识别为路径的参数名（哪怕值已是完整路径、不需要改写也记下来）：
    // 有它才能从日志区分「没识别出路径参数」与「识别了但值不是裸名」。
    let mut path_keys: Vec<&str> = Vec::new();

    for (key, schema) in properties {
        let Some(value) = object.get_mut(key.as_str()) else {
            continue;
        };
        // 是否把**字符串**当文件名补全，看字段名；但所有字段都要往下递归 ——
        // 参数包（如 `{"output": {"path": "a.png"}}`）的外层名字不带路径语义。
        let key_is_path = is_path_key(key);
        if key_is_path {
            path_keys.push(key.as_str());
        }
        rewrite_in_place(value, schema, key, key_is_path, output_dir, &mut rewrites);
    }

    // 无论是否改写都记一条：诊断「为什么没生效」全靠它。
    tracing::debug!(
        target: "tool_path_normalization",
        tool = %tool.name,
        output_dir = %output_dir.display(),
        path_keys = %path_keys.join(","),
        rewrites = rewrites.len(),
        "工具入参的路径判定（path_keys=识别为路径的参数名，rewrites=实际补全条数）"
    );

    (arguments, rewrites)
}

/// 字段名是否像「本地文件路径」。
///
/// 用**精确名 + 后缀**，而不是「名字里含 `path`」—— 后者会把 `xpath` 也算成路径。
fn is_path_key(name: &str) -> bool {
    const EXACT: &[&str] = &[
        "filename",
        "file_name",
        "filepath",
        "file_path",
        "path",
        "file",
        "dir",
        "directory",
        "folder",
        "paths",
        "files",
    ];
    EXACT.contains(&name)
        || name.ends_with("_path")
        || name.ends_with("_file")
        || name.ends_with("_dir")
        || name.ends_with("_filename")
}

/// 就地把值里的「裸文件名」补成产出目录下的完整路径。
fn rewrite_in_place(
    value: &mut Value,
    schema: &Value,
    path: &str,
    key_is_path: bool,
    output_dir: &Path,
    out: &mut Vec<PathRewrite>,
) {
    match value {
        // 字段名不像路径 → 字符串原样保留（否则 `text` / `selector` 都会被当路径改）。
        Value::String(_) if !key_is_path => {}
        Value::String(raw) => {
            let Some(full) = resolve_bare_name(raw, output_dir) else {
                return;
            };
            out.push(PathRewrite {
                key: path.to_string(),
                from: raw.clone(),
                to: full.clone(),
            });
            *raw = full;
        }
        Value::Array(items) => {
            // 元素用的 schema 是 `items`，不是数组本身的 schema；
            // 元素沿用**同一个字段名**的判断（`paths: ["a.png"]`）。
            let item_schema = schema.get("items").unwrap_or(schema);
            for (index, item) in items.iter_mut().enumerate() {
                rewrite_in_place(
                    item,
                    item_schema,
                    &format!("{path}[{index}]"),
                    key_is_path,
                    output_dir,
                    out,
                );
            }
        }
        Value::Object(map) => {
            let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
                return;
            };
            for (key, child) in map.iter_mut() {
                if let Some(child_schema) = properties.get(key.as_str()) {
                    rewrite_in_place(
                        child,
                        child_schema,
                        &format!("{path}.{key}"),
                        is_path_key(key),
                        output_dir,
                        out,
                    );
                }
            }
        }
        _ => {}
    }
}

/// 裸文件名 → 产出目录下的完整路径；不是裸名则返回 `None`（表示「不要动它」）。
fn resolve_bare_name(raw: &str, output_dir: &Path) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // `./a.png` 与 `a.png` 等价：只去掉开头这一个冗余前缀。
    let bare = trimmed
        .strip_prefix("./")
        .or_else(|| trimmed.strip_prefix(".\\"))
        .unwrap_or(trimmed);
    // 含任何路径成分就不是裸名 —— 交回工具按它自己的基准解析，我们无权覆盖。
    if bare.is_empty() || bare.contains('/') || bare.contains('\\') || bare.contains(':') {
        return None;
    }
    Some(output_dir.join(bare).to_string_lossy().into_owned())
}

/// 错误文本是否像「文件 / 路径找不到，或被目录白名单拒了」。
///
/// 用途：调用点据此决定要不要**用模型给的原参数回退重试一次**。
///
/// ⚠️ 关键词必须**具体到路径** —— 回退会**再执行一次工具**，而工具可能带副作用
/// （截图、写文件、跑命令）。所以 `not found` / `denied` 这类宽词不能用：
/// 它们会命中 `Command not found`、`element not found`、`permission denied`，
/// 把「与路径无关的失败」也变成一次重跑。
pub fn looks_like_path_error(text: &str) -> bool {
    const KEYS: &[&str] = &[
        // 文件 / 目录不存在
        "no such file", // POSIX ENOENT 的固定搭配
        "enoent",
        "does not exist",
        "file not found",
        "path not found",
        "cannot find the file",
        "cannot find the path",
        // 被目录白名单拒（playwright：`File access denied: X is outside allowed roots`）
        "outside allowed",
        "path_outside_allowed",
        // 中文（同样取具体短语，避免「找不到元素」这类）
        "找不到指定的路径",
        "找不到指定的文件",
        "文件不存在",
        "不在允许目录内",
    ];
    let lowered = text.to_lowercase();
    KEYS.iter().any(|key| lowered.contains(key))
}

/// 回灌文案：告诉模型「参数被动过」（治「改写静默」）。
///
/// `used_rewritten` = 本次调用**最终用的是补全后的参数**，还是模型给的原参数：
/// 补全后失败、回退用原参数重试成功时，必须如实说「用的是你给的原值」——
/// 否则文案与实际路径矛盾，反而制造新的幻觉源。
///
/// 只在**确实发生过改写**时调用（调用点保证）；不含工具名，也不注入本步意图。
pub fn render_rewrites(rewrites: &[PathRewrite], used_rewritten: bool) -> String {
    let mut rendered = String::from(if used_rewritten {
        "\n\n（宿主提示：以下参数的相对文件名已按本次执行的产出目录补全为完整路径："
    } else {
        "\n\n（宿主提示：以下参数本已被补全为完整路径，但补全后调用失败，已改用你给的原值："
    });
    for rewrite in rewrites {
        rendered.push_str(&format!(
            "\n- `{}`：`{}` → `{}`",
            rewrite.key, rewrite.from, rewrite.to
        ));
    }
    rendered.push('）');
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn tool_with(schema: Value) -> Tool {
        Tool {
            name: "demo".to_string(),
            description: String::new(),
            input_schema: schema,
        }
    }

    fn out_dir() -> PathBuf {
        PathBuf::from("D:/out")
    }

    /// 期望的补全结果。**必须用 `join` 生成** —— 分隔符随平台变（Windows 上 `join` 给 `\`）。
    fn bare(expected: &str) -> String {
        out_dir().join(expected).to_string_lossy().into_owned()
    }

    #[test]
    fn path_key_does_not_swallow_xpath_or_selector() {
        assert!(is_path_key("filename"));
        assert!(is_path_key("path"));
        assert!(is_path_key("save_path"));
        assert!(is_path_key("paths"));
        // 非文件路径：不能命中，否则会把 selector / URL 也当路径改。
        assert!(!is_path_key("xpath"));
        assert!(!is_path_key("selector"));
        assert!(!is_path_key("url"));
        assert!(!is_path_key("query"));
        assert!(!is_path_key("text"));
    }

    #[test]
    fn bare_name_accepts_only_a_file_name() {
        assert_eq!(resolve_bare_name("a.png", &out_dir()), Some(bare("a.png")));
        // 开头的 `./` 是冗余前缀，仍算裸名。
        assert_eq!(resolve_bare_name("./a.png", &out_dir()), Some(bare("a.png")));
        assert_eq!(resolve_bare_name(" a.png ", &out_dir()), Some(bare("a.png")));
        // 含路径成分 → 不碰（避免双重拼接 / 覆盖模型明确的意图）。
        assert_eq!(resolve_bare_name("sub/a.png", &out_dir()), None);
        assert_eq!(resolve_bare_name("..\\a.png", &out_dir()), None);
        assert_eq!(resolve_bare_name("D:\\a.png", &out_dir()), None);
        assert_eq!(resolve_bare_name("", &out_dir()), None);
        assert_eq!(resolve_bare_name("   ", &out_dir()), None);
    }

    #[test]
    fn rewrites_bare_filename_from_schema() {
        let schema = json!({"type":"object","properties":{"filename":{"type":"string"}}});
        let (args, rewrites) = resolve(
            Some(&tool_with(schema)),
            json!({"filename": "captcha-1.png", "target": "e31"}),
            &out_dir(),
        );
        assert_eq!(args["filename"], json!(bare("captcha-1.png")));
        // 非路径参数原样不动。
        assert_eq!(args["target"], json!("e31"));
        assert_eq!(
            rewrites,
            vec![PathRewrite {
                key: "filename".to_string(),
                from: "captcha-1.png".to_string(),
                to: bare("captcha-1.png"),
            }]
        );
    }

    #[test]
    fn keeps_values_that_already_carry_a_path() {
        let schema = json!({"type":"object","properties":{"filename":{"type":"string"}}});
        let (args, rewrites) = resolve(
            Some(&tool_with(schema)),
            json!({"filename": "data/cache/x/a.png"}),
            &out_dir(),
        );
        assert_eq!(args["filename"], json!("data/cache/x/a.png"));
        assert!(rewrites.is_empty());
    }

    #[test]
    fn ignores_non_path_keys() {
        let schema = json!({"type":"object","properties":{"text":{"type":"string"}}});
        let (args, rewrites) = resolve(
            Some(&tool_with(schema)),
            json!({"text": "a.png"}),
            &out_dir(),
        );
        assert_eq!(args["text"], json!("a.png"));
        assert!(rewrites.is_empty());
    }

    #[test]
    fn without_schema_nothing_changes() {
        let (args, rewrites) = resolve(None, json!({"filename": "a.png"}), &out_dir());
        assert_eq!(args["filename"], json!("a.png"));
        assert!(rewrites.is_empty());

        // 有工具但 schema 里没有 properties（如 additionalProperties: true）。
        let passthrough = tool_with(json!({"type":"object"}));
        let (args, rewrites) = resolve(Some(&passthrough), json!({"filename": "a.png"}), &out_dir());
        assert_eq!(args["filename"], json!("a.png"));
        assert!(rewrites.is_empty());
    }

    /// 产出目录为空时**不能**记「假改写」：`"".join("a.png")` 仍是 `a.png`，
    /// 补了等于没补，却会污染回灌文案与日志。
    #[test]
    fn empty_output_dir_is_not_a_rewrite() {
        let schema = json!({"type":"object","properties":{"filename":{"type":"string"}}});
        let (args, rewrites) = resolve(
            Some(&tool_with(schema)),
            json!({"filename": "a.png"}),
            Path::new(""),
        );
        assert_eq!(args["filename"], json!("a.png"));
        assert!(rewrites.is_empty());
    }

    #[test]
    fn rewrites_array_items_individually() {
        let schema = json!({
            "type":"object",
            "properties":{"paths":{"type":"array","items":{"type":"string"}}}
        });
        let (args, rewrites) = resolve(
            Some(&tool_with(schema)),
            json!({"paths": ["a.png", "sub/b.png"]}),
            &out_dir(),
        );
        assert_eq!(args["paths"], json!([bare("a.png"), "sub/b.png"]));
        assert_eq!(rewrites.len(), 1);
        assert_eq!(rewrites[0].key, "paths[0]");
    }

    #[test]
    fn rewrites_nested_object_field() {
        let schema = json!({
            "type":"object",
            "properties":{"output":{"type":"object","properties":{"path":{"type":"string"}}}}
        });
        let (args, rewrites) = resolve(
            Some(&tool_with(schema)),
            json!({"output": {"path": "a.png"}}),
            &out_dir(),
        );
        assert_eq!(args["output"]["path"], json!(bare("a.png")));
        assert_eq!(rewrites[0].key, "output.path");
    }

    #[test]
    fn only_path_like_errors_trigger_fallback() {
        assert!(looks_like_path_error(
            "File access denied: D:\\x is outside allowed roots"
        ));
        assert!(looks_like_path_error("Error: ENOENT: no such file or directory"));
        assert!(looks_like_path_error("访问被拒绝：文件不存在"));
        assert!(looks_like_path_error("系统找不到指定的文件。"));
        // 与路径无关的失败**不能**触发重试 —— 否则带副作用的工具会被白跑一次。
        assert!(!looks_like_path_error("Cannot type text into input[type=number]"));
        assert!(!looks_like_path_error("Command not found: foo"));
        assert!(!looks_like_path_error("element not found: e31"));
        assert!(!looks_like_path_error("permission denied by policy"));
    }

    #[test]
    fn rewrite_note_does_not_name_the_tool() {
        let rewrites = vec![PathRewrite {
            key: "filename".to_string(),
            from: "a.png".to_string(),
            to: "D:/out/a.png".to_string(),
        }];
        let note = render_rewrites(&rewrites, true);
        assert!(note.contains("D:/out/a.png"));
        assert!(note.contains("filename"));
        // 纪律：不点名工具，避免工具改名 / 未注册时变成幻觉源。
        assert!(!note.contains("browser_"));
        assert!(!note.contains("demo"));

        // 回退用原参数时不能说「已补全」，否则文案与实际路径矛盾。
        let fallback_note = render_rewrites(&rewrites, false);
        assert!(fallback_note.contains("已改用你给的原值"));
        assert!(!fallback_note.contains("已按本次执行的产出目录补全"));
    }
}
