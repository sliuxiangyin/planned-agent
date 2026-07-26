//! 内容格式检测
//!
//! 提供一组**纯启发式 + 严格 parse 的混合**判定函数，用于识别文本内容的格式：
//! - [`ContentFormat`] 枚举：所有可识别的格式
//! - [`detect`]：综合判定（按 JSON → HTML → XML → Markdown → CSV → Text 优先级）
//! - 独立的 `is_*` 函数：按需快速判定
//!
//! ## 设计原则
//! - **不调 LLM** —— 保持纯函数、零依赖、可单测。
//! - **严格优先于宽松** —— JSON / XML 用 parser 严格检查，启发式只兜底。
//! - **保守判定**：歧义内容默认归 [`ContentFormat::Text`]，避免误判为结构化格式。
//! - **可组合**：每个 `is_*` 函数都公开，调用方可独立使用，不需要 `detect` 入口。

/// 内容格式枚举。
///
/// 字段顺序就是 [`detect`] 的判定优先级顺序（更具体的在前）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentFormat {
    /// JSON 对象或数组
    Json,
    /// HTML（含 DOCTYPE / 常见标签密度）
    Html,
    /// XML（`<?xml` prolog 或 根元素闭合对）
    Xml,
    /// Markdown（ATX/Setext 标题、围栏代码块等）
    Markdown,
    /// CSV（多行、每行相同逗号数）
    Csv,
    /// 纯文本（兜底）
    Text,
    /// 空内容或无法分类
    Unknown,
}

impl ContentFormat {
    /// 中文 / 人类可读标签
    pub fn label(&self) -> &'static str {
        match self {
            ContentFormat::Json => "JSON",
            ContentFormat::Html => "HTML",
            ContentFormat::Xml => "XML",
            ContentFormat::Markdown => "Markdown",
            ContentFormat::Csv => "CSV",
            ContentFormat::Text => "纯文本",
            ContentFormat::Unknown => "未知",
        }
    }

    /// 是否为结构化格式（JSON / XML / CSV）
    pub fn is_structured(&self) -> bool {
        matches!(
            self,
            ContentFormat::Json | ContentFormat::Xml | ContentFormat::Csv
        )
    }

    /// 是否为标记类格式（HTML / Markdown / XML）—— 富文本
    pub fn is_markup(&self) -> bool {
        matches!(
            self,
            ContentFormat::Html | ContentFormat::Markdown | ContentFormat::Xml
        )
    }
}

// =====================================================================
// 综合判定入口
// =====================================================================

/// 综合判定给定内容的最可能格式。
///
/// 优先级顺序（更具体的优先）：
/// 1. JSON —— `{` / `[` 起头 + `serde_json` 解析成功
/// 2. HTML —— `<!doctype>` / `<html>` / 高标签密度 + 常见标签
/// 3. XML —— `<?xml>` prolog 或 `<tag>...</tag>` 闭合对
/// 4. Markdown —— 标题 / 代码块
/// 5. CSV —— 多行 + 每行相同逗号数
/// 6. Text —— 兜底
pub fn detect(content: &str) -> ContentFormat {
    if content.trim().is_empty() {
        return ContentFormat::Unknown;
    }

    if is_json(content) {
        return ContentFormat::Json;
    }
    if is_html(content) {
        return ContentFormat::Html;
    }
    if is_xml(content) {
        return ContentFormat::Xml;
    }
    if is_markdown(content) {
        return ContentFormat::Markdown;
    }
    if is_csv(content) {
        return ContentFormat::Csv;
    }
    ContentFormat::Text
}

// =====================================================================
// 独立的判定函数
// =====================================================================

/// 是否为合法 JSON。
///
/// 首字符必须为 `{` 或 `[`（去除前导空白后），且 `serde_json::from_str` 解析成功。
pub fn is_json(s: &str) -> bool {
    let trimmed = s.trim_start();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
}

/// 是否疑似为 HTML。
///
/// 启发式规则（与 `html_clean_subagent::looks_like_html` 同源，独立可调用）：
/// - `<!doctype>` / `<?xml>` / `<html>` 起头 → true
/// - 头部 512 字符内 ≥ 3 个 `<` 和 ≥ 3 个 `>` + 命中常见标签列表 → true
/// - 长度 < 64 → false
/// - 其他 → false
pub fn is_html(s: &str) -> bool {
    let trimmed = s.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with("<!doctype")
        || trimmed.starts_with("<!DOCTYPE")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<HTML")
    {
        return true;
    }
    // XML 处理指令仅当根元素封闭才升级为 HTML
    if trimmed.starts_with("<?xml") {
        return false;
    }

    if trimmed.len() < 64 {
        return false;
    }

    let head: String = trimmed.chars().take(512).collect();
    let opens = head.matches('<').count();
    let closes = head.matches('>').count();
    if opens < 3 || closes < 3 {
        return false;
    }

    const COMMON_TAGS: &[&str] = &[
        "<div", "<DIV", "<p ", "<P ", "<p>", "<P>", "<a ", "<A ", "<a>", "<A>",
        "<span", "<SPAN", "<article", "<ARTICLE", "<section", "<SECTION",
        "<header", "<HEADER", "<footer", "<FOOTER", "<nav", "<NAV",
        "<table", "<TABLE", "<ul", "<UL", "<ol", "<OL", "<li", "<LI",
        "<h1", "<h2", "<h3", "<h4", "<h5", "<h6",
        "<img", "<IMG", "<main", "<MAIN", "<body", "<BODY", "<head", "<HEAD",
    ];
    COMMON_TAGS.iter().any(|t| head.contains(t))
}

/// 是否疑似为 XML（但不是 HTML）。
pub fn is_xml(s: &str) -> bool {
    let trimmed = s.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with("<?xml") {
        return true;
    }
    // 启发式：以 `<` 起头 + 同时存在开 / 闭标签对（首字符 `<`）
    if trimmed.starts_with('<') {
        // 取第一个 50 字符内是否含 `</`
        let head: String = trimmed.chars().take(50).collect();
        if head.contains("</") {
            return true;
        }
    }
    false
}

/// 是否疑似为 Markdown。
///
/// 启发式规则：
/// - 第一行以 `#`/`##`/`###`/`####`/`#####`/`######` + 空格 / EOF 起头（ATX 标题）
/// - 第二行全是 `=` 或 `-` 字符（Setext 标题）
/// - 任意位置出现 ``` 代码块围栏
pub fn is_markdown(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // 围栏代码块
    if s.contains("```") {
        return true;
    }

    let mut lines = s.lines();
    if let Some(first) = lines.next() {
        let first = first.trim_start();
        // ATX 标题：# ~ ###### + 空格或行末
        let hashes = first.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes) {
            let rest = &first[hashes..];
            if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                return true;
            }
        }
    }
    // Setext 标题：第一行是文本 + 第二行全是 = 或 -
    let lines: Vec<&str> = s.lines().take(2).collect();
    if lines.len() == 2 {
        let underline = lines[1].trim();
        if underline.len() >= 3 {
            if underline.chars().all(|c| c == '=') {
                return true;
            }
            if underline.chars().all(|c| c == '-') {
                // 注：Setext 用 `-` 会和水平分割线冲突；如果后面有空行+正文可能是真正分割线。
                // 但 Setext 标题要求第一行非空 + 第二行纯 `-`，这里仍接受。
                return true;
            }
        }
    }

    false
}

/// 是否疑似为 CSV。
///
/// 启发式规则：
/// - 至少 2 行非空内容
/// - 每行非空行的逗号数相同（且 ≥ 1）
///
/// 注：Markdown 列表可能也含逗号，但通常行内逗号数不一致，不会被误判。
pub fn is_csv(s: &str) -> bool {
    let lines: Vec<&str> = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() < 2 {
        return false;
    }
    let first_count = lines[0].matches(',').count();
    if first_count < 1 {
        return false;
    }
    lines.iter().all(|l| l.matches(',').count() == first_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============== detect 综合判定 ==============

    #[test]
    fn detect_empty_is_unknown() {
        assert_eq!(detect(""), ContentFormat::Unknown);
        assert_eq!(detect("   \n  "), ContentFormat::Unknown);
    }

    #[test]
    fn detect_json_object() {
        assert_eq!(detect(r#"{"a":1,"b":[1,2,3]}"#), ContentFormat::Json);
    }

    #[test]
    fn detect_json_array() {
        assert_eq!(detect("[1,2,3]"), ContentFormat::Json);
    }

    #[test]
    fn detect_html_with_doctype() {
        assert_eq!(
            detect("<!doctype html><html><body><h1>Title</h1></body></html>"),
            ContentFormat::Html
        );
    }

    #[test]
    fn detect_html_with_common_tags() {
        let long_html = format!(
            "<article><h1>{}</h1><p>this is some body text with more than enough characters for detection</p></article>",
            "Title"
        );
        assert_eq!(detect(&long_html), ContentFormat::Html);
    }

    #[test]
    fn detect_xml_with_prolog() {
        assert_eq!(detect(r#"<?xml version="1.0"?><root><a>1</a></root>"#), ContentFormat::Xml);
    }

    #[test]
    fn detect_markdown_with_atx_heading() {
        assert_eq!(detect("# Hello World\nSome body"), ContentFormat::Markdown);
    }

    #[test]
    fn detect_markdown_with_setext_heading() {
        assert_eq!(detect("Hello World\n==========="), ContentFormat::Markdown);
    }

    #[test]
    fn detect_markdown_with_code_fence() {
        assert_eq!(
            detect("Some intro\n```rust\nfn main() {}\n```\nMore"),
            ContentFormat::Markdown
        );
    }

    #[test]
    fn detect_csv_uniform_columns() {
        assert_eq!(
            detect("name,age\nAlice,30\nBob,25"),
            ContentFormat::Csv
        );
    }

    #[test]
    fn detect_plain_text_falls_through() {
        assert_eq!(detect("just some normal English text."), ContentFormat::Text);
    }

    // ============== is_json ==============

    #[test]
    fn is_json_object() {
        assert!(is_json(r#"{"a":1}"#));
    }

    #[test]
    fn is_json_with_leading_whitespace() {
        assert!(is_json("   \n\t[1,2]"));
    }

    #[test]
    fn is_json_negative_text() {
        assert!(!is_json("hello"));
    }

    #[test]
    fn is_json_negative_malformed() {
        assert!(!is_json("{not valid json"));
    }

    // ============== is_html ==============

    #[test]
    fn is_html_doctype() {
        assert!(is_html("<!doctype html><html></html>"));
    }

    #[test]
    fn is_html_doctype_case_insensitive() {
        assert!(is_html("<!DOCTYPE html><html></html>"));
    }

    #[test]
    fn is_html_short_text_fails() {
        assert!(!is_html("<p>tiny</p>"));
    }

    #[test]
    fn is_html_long_text_with_tags_passes() {
        let s = "<article><p>Some content with multiple elements and tags here</p></article>";
        assert!(is_html(s));
    }

    #[test]
    fn is_html_xml_prolog_not_html() {
        assert!(!is_html(r#"<?xml version="1.0"?><root><a>1</a></root>"#));
    }

    // ============== is_xml ==============

    #[test]
    fn is_xml_with_prolog() {
        assert!(is_xml(r#"<?xml version="1.0"?><root><a>1</a></root>"#));
    }

    #[test]
    fn is_xml_with_closing_pair() {
        assert!(is_xml("<root><child>1</child></root>"));
    }

    // ============== is_markdown ==============

    #[test]
    fn is_markdown_atx_h1() {
        assert!(is_markdown("# Title"));
    }

    #[test]
    fn is_markdown_atx_h3() {
        assert!(is_markdown("### Title"));
    }

    #[test]
    fn is_markdown_setext_h1() {
        assert!(is_markdown("Title\n====="));
    }

    #[test]
    fn is_markdown_setext_h2() {
        assert!(is_markdown("Title\n-----"));
    }

    #[test]
    fn is_markdown_code_fence() {
        assert!(is_markdown("intro\n```\ncode\n```"));
    }

    #[test]
    fn is_markdown_plain_text_negative() {
        assert!(!is_markdown("just a normal paragraph"));
    }

    // ============== is_csv ==============

    #[test]
    fn is_csv_two_uniform_columns() {
        assert!(is_csv("name,age\nAlice,30"));
    }

    #[test]
    fn is_csv_three_columns() {
        assert!(is_csv("a,b,c\n1,2,3\n4,5,6"));
    }

    #[test]
    fn is_csv_single_row_negative() {
        assert!(!is_csv("name,age"));
    }

    #[test]
    fn is_csv_misaligned_negative() {
        assert!(!is_csv("a,b,c\n1,2\n3,4,5"));
    }

    #[test]
    fn is_csv_no_commas_negative() {
        assert!(!is_csv("hello\nworld"));
    }

    // ============== ContentFormat 工具方法 ==============

    #[test]
    fn content_format_label() {
        assert_eq!(ContentFormat::Html.label(), "HTML");
        assert_eq!(ContentFormat::Text.label(), "纯文本");
    }

    #[test]
    fn content_format_is_structured() {
        assert!(ContentFormat::Json.is_structured());
        assert!(ContentFormat::Xml.is_structured());
        assert!(ContentFormat::Csv.is_structured());
        assert!(!ContentFormat::Html.is_structured());
        assert!(!ContentFormat::Text.is_structured());
    }

    #[test]
    fn content_format_is_markup() {
        assert!(ContentFormat::Html.is_markup());
        assert!(ContentFormat::Markdown.is_markup());
        assert!(ContentFormat::Xml.is_markup());
        assert!(!ContentFormat::Json.is_markup());
    }
}