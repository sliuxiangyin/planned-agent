//! `output_schema`（输出契约）的形态定义、校验与解析。
//!
//! 契约（与 `prompts/flexible/flexible_output.toml` 一致）：
//! - `kind` 决定结果形态，六值之一：`bool` / `text` / `markdown` / `json` / `csv` / `file`；
//! - `bool` 必须有非空 `success`（只有成败，没有交付内容）；其余 kind 必须有非空 `goal`；
//! - `format` 是形态细节；`required` / `wanted` 只在 `json` / `csv` 下才有意义；
//! - 文本字段（`goal` / `success` / `format`）里出现的参数值必须写 `${name}`
//!   （收集与替换见 [`crate::flexible::placeholder`]）。
//!
//! 契约整体可以是 `null`：用户跳过了输出定义步，或明确表示「现在还定不了」——
//! 此时执行器不做输出整理，直接以交付步的输出作为结果（见 `executor.rs`）。
//!
//! 这是契约的**唯一定义处**：保存落库前的校验、执行期的输出整理、GUI 的面板展示都走这里。

use serde_json::Value;

/// 结果形态（六值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    /// 只要「成功 / 失败」判断，没有交付内容。
    Bool,
    /// 一段自由文本。
    Text,
    /// 一份 Markdown 文档。
    Markdown,
    /// 一个 JSON 对象。
    Json,
    /// 一份表格（CSV 文本）。
    Csv,
    /// 一个落盘文件（以路径 + 说明交付）。
    File,
}

impl OutputKind {
    /// 全部合法取值，按契约顺序（提示词里也按这个顺序列举）。
    pub const ALL: [OutputKind; 6] = [
        OutputKind::Bool,
        OutputKind::Text,
        OutputKind::Markdown,
        OutputKind::Json,
        OutputKind::Csv,
        OutputKind::File,
    ];

    /// 解析 `kind` 字符串；未知取值返回 `None`（由调用方决定报错还是兜底）。
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "bool" => Some(OutputKind::Bool),
            "text" => Some(OutputKind::Text),
            "markdown" => Some(OutputKind::Markdown),
            "json" => Some(OutputKind::Json),
            "csv" => Some(OutputKind::Csv),
            "file" => Some(OutputKind::File),
            _ => None,
        }
    }

    /// 契约里的字符串表示。
    pub fn as_str(self) -> &'static str {
        match self {
            OutputKind::Bool => "bool",
            OutputKind::Text => "text",
            OutputKind::Markdown => "markdown",
            OutputKind::Json => "json",
            OutputKind::Csv => "csv",
            OutputKind::File => "file",
        }
    }

    /// 是否只有成败、没有交付内容。
    pub fn is_bool(self) -> bool {
        matches!(self, OutputKind::Bool)
    }

    /// 是否使用字段清单（`required` / `wanted`）—— 只有结构化输出才谈得上「字段」。
    pub fn uses_fields(self) -> bool {
        matches!(self, OutputKind::Json | OutputKind::Csv)
    }
}

/// 校验通过后的输出契约视图（保存 / 执行 / 展示共用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSchema {
    /// 结果形态。
    pub kind: OutputKind,
    /// 要交付什么（`bool` 之外必填）。
    pub goal: Option<String>,
    /// 什么算成功（`bool` 必填）。
    pub success: Option<String>,
    /// 形态细节（编码 / 表头 / 单位 / 文件类型等）。
    pub format: Option<String>,
    /// 能担保存在的字段（仅 `json` / `csv`）。
    pub required: Vec<String>,
    /// 探索型字段（仅 `json` / `csv`）。
    pub wanted: Vec<String>,
}

impl OutputSchema {
    /// 严格解析：结构、`kind` 合法性、按 kind 的必填项都校验，任一不满足即 `Err`。
    ///
    /// 落库前（`save_callback`）与执行期（输出整理步之前）都用它把关。
    pub fn parse(value: &Value) -> Result<Self, String> {
        let Some(object) = value.as_object() else {
            return Err("output_schema 必须是对象".to_string());
        };

        let kind_text = object
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(kind) = OutputKind::parse(kind_text) else {
            return Err(format!(
                "output_schema.kind 非法: {:?}（应为 {}）",
                kind_text,
                OutputKind::ALL
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ));
        };

        // 空白字符串等同于「没写」：避免 `"goal": " "` 混过必填校验。
        let text_field = |name: &str| -> Option<String> {
            object
                .get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };

        let goal = text_field("goal");
        let success = text_field("success");
        if kind.is_bool() {
            if success.is_none() {
                return Err("output_schema.kind 为 bool 时必须给出非空 success".to_string());
            }
        } else if goal.is_none() {
            return Err(format!(
                "output_schema.kind 为 {} 时必须给出非空 goal",
                kind.as_str()
            ));
        }

        // 字段清单只对结构化输出生效：别的 kind 上写了也一律忽略（契约只承认 json / csv 有字段）
        let (required, wanted) = if kind.uses_fields() {
            let required = string_list(object, "required");
            // 同名同时出现在两个清单里没有意义；`required` 更强，故从 `wanted` 里剔除
            let wanted = string_list(object, "wanted")
                .into_iter()
                .filter(|name| !required.contains(name))
                .collect();
            (required, wanted)
        } else {
            (Vec::new(), Vec::new())
        };

        Ok(Self {
            kind,
            goal,
            success,
            format: text_field("format"),
            required,
            wanted,
        })
    }
}

/// 读一个字符串数组字段：去空白、丢非字符串项、保序去重。
fn string_list(object: &serde_json::Map<String, Value>, name: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let Some(items) = object.get(name).and_then(Value::as_array) else {
        return names;
    };
    for item in items {
        let Some(name) = item.as_str().map(str::trim).filter(|text| !text.is_empty()) else {
            continue;
        };
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_string());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_bool_contract() {
        let schema = OutputSchema::parse(&json!({
            "kind": "bool",
            "goal": "向指定文件追加一行",
            "success": "文件末尾新增一行即视为成功",
        }))
        .unwrap();
        assert_eq!(schema.kind, OutputKind::Bool);
        assert!(schema.kind.is_bool());
        assert!(!schema.kind.uses_fields());
        assert!(schema.required.is_empty() && schema.wanted.is_empty());
    }

    #[test]
    fn bool_requires_success() {
        let err = OutputSchema::parse(&json!({ "kind": "bool", "goal": "g" })).unwrap_err();
        assert!(err.contains("success"), "{err}");
    }

    #[test]
    fn non_bool_requires_goal() {
        let err = OutputSchema::parse(&json!({ "kind": "csv", "success": "s" })).unwrap_err();
        assert!(err.contains("goal"), "{err}");
        // 纯空白不算写了
        let err = OutputSchema::parse(&json!({ "kind": "csv", "goal": "  " })).unwrap_err();
        assert!(err.contains("goal"), "{err}");
    }

    #[test]
    fn rejects_unknown_kind_and_non_object() {
        let err = OutputSchema::parse(&json!({ "kind": "success_only", "success": "s" })).unwrap_err();
        assert!(err.contains("success_only"), "{err}");
        assert!(err.contains("bool"), "错误里应列出合法取值: {err}");
        assert!(OutputSchema::parse(&json!("csv")).is_err());
        assert!(OutputSchema::parse(&Value::Null).is_err());
    }

    #[test]
    fn field_lists_only_apply_to_structured_kinds() {
        // json 保留字段清单，并做去重 + required/wanted 互斥
        let schema = OutputSchema::parse(&json!({
            "kind": "json",
            "goal": "抽字段",
            "required": ["title", "title", " "],
            "wanted": ["stock", "title"],
        }))
        .unwrap();
        assert_eq!(schema.required, vec!["title".to_string()]);
        assert_eq!(schema.wanted, vec!["stock".to_string()]);

        // text 上写了字段清单也一律忽略
        let schema = OutputSchema::parse(&json!({
            "kind": "text",
            "goal": "写一段总结",
            "required": ["a"],
            "wanted": ["b"],
        }))
        .unwrap();
        assert!(schema.required.is_empty() && schema.wanted.is_empty());
    }

    #[test]
    fn kind_round_trips_through_str() {
        for kind in OutputKind::ALL {
            assert_eq!(OutputKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(OutputKind::parse(" success_only "), None);
    }
}
