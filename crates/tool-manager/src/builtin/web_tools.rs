use std::sync::Arc;
use async_trait::async_trait;
use anyhow::Result;
use serde_json::{json, Value};
use planned_agent_core::types::{Tool, ToolResult};
use planned_agent_core::tool_registry::{ToolExecutor, ToolCategory, BuiltinToolProvider};

const DEFAULT_MAX_CHARS: usize = 20_000;
const MAX_INPUT_BYTES: usize = 5 * 1024 * 1024; // 5 MiB
const ABSOLUTE_MAX_CHARS: usize = 100_000;

/// 内置网页清洗工具提供者
pub struct WebToolsProvider;

impl BuiltinToolProvider for WebToolsProvider {
    fn tools(&self) -> Vec<(Tool, Vec<ToolCategory>)> {
        vec![(
            Tool {
                name: "builtin_clean_html".to_string(),
                description: "Clean HTML web page content, output as plain text or Markdown. Strips navigation, ads, scripts and other noise before AI analysis to prevent context explosion (built-in tool)".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "html": {
                            "type": "string",
                            "description": "Raw HTML content to be cleaned"
                        },
                        "format": {
                            "type": "string",
                            "enum": ["text", "markdown"],
                            "description": "Output format: 'text' for plain text, 'markdown' to preserve headings, lists, links and other structures while keeping URLs",
                            "default": "text"
                        },
                        "max_chars": {
                            "type": "integer",
                            "description": "Maximum number of characters in the output, default 20000, hard limit 100000",
                            "default": 20000,
                            "minimum": 1,
                            "maximum": 100000
                        }
                    },
                    "required": ["html"]
                }),
            },
            vec![ToolCategory::Browser, ToolCategory::Text],
        )]
    }

    fn executor(&self) -> Arc<dyn ToolExecutor> {
        Arc::new(WebToolsExecutor)
    }
}

struct WebToolsExecutor;

#[async_trait]
impl ToolExecutor for WebToolsExecutor {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        match tool_name {
            "builtin_clean_html" => clean_html(arguments),
            _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name)),
        }
    }

    fn name(&self) -> &str {
        "builtin_web_tools"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec!["builtin_clean_html".to_string()]
    }
}

fn clean_html(arguments: Value) -> Result<ToolResult> {
    let html = arguments["html"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing required field 'html'"))?;

    if html.trim().is_empty() {
        return Err(anyhow::anyhow!("Input 'html' is empty"));
    }

    if html.len() > MAX_INPUT_BYTES {
        return Err(anyhow::anyhow!(
            "Input HTML exceeds 5 MiB limit ({} bytes)",
            html.len()
        ));
    }

    let format = arguments["format"]
        .as_str()
        .unwrap_or("text");

    let max_chars = arguments["max_chars"]
        .as_u64()
        .map(|v| v as usize)
        .unwrap_or(DEFAULT_MAX_CHARS)
        .clamp(1, ABSOLUTE_MAX_CHARS);

    let original_chars = html.chars().count();

    // 正文优先提取
    let (article_title, content_html, extraction_method) =
        try_readability(html).unwrap_or_else(|| {
            (
                None,
                html.to_string(),
                "full_page".to_string(),
            )
        });

    // 根据模式转换
    let mut cleaned = match format {
        "markdown" => html_to_markdown(&content_html),
        "text" => html_to_text(&content_html),
        _ => return Err(anyhow::anyhow!("Unknown format '{}', expected 'text' or 'markdown'", format)),
    };

    // 裁剪首尾空白、压缩连续空行
    cleaned = normalize_whitespace(&cleaned);

    let cleaned_chars = cleaned.chars().count();
    let truncated = cleaned_chars > max_chars;

    if truncated {
        cleaned = truncate_at_boundary(&cleaned, max_chars);
    }

    let returned_chars = cleaned.chars().count();

    let mut result = json!({
        "content": cleaned,
        "format": format,
        "extraction_method": extraction_method,
        "original_chars": original_chars,
        "cleaned_chars": cleaned_chars,
        "returned_chars": returned_chars,
        "truncated": truncated,
    });

    if let Some(title) = article_title {
        result["title"] = Value::String(title);
    }

    Ok(ToolResult {
        call_id: uuid::Uuid::new_v4().to_string(),
        content: result,
        is_error: false,
    })
}

/// 尝试使用 readability 提取正文
/// 返回 (title, content_html, method) 或 None 表示降级
fn try_readability(html: &str) -> Option<(Option<String>, String, String)> {
    use readability_rust::Readability;

    let mut parser = match Readability::new(html, None) {
        Ok(p) => p,
        Err(_) => return None,
    };

    match parser.parse() {
        Some(article) => {
            let content = article.content.as_deref().unwrap_or("");
            if content.trim().is_empty() {
                return None;
            }
            Some((
                article.title,
                content.to_string(),
                "readability".to_string(),
            ))
        }
        None => None,
    }
}

/// HTML 转 Markdown，保留 URL
fn html_to_markdown(html: &str) -> String {
    htmd::convert(html).unwrap_or_else(|_| {
        // 降级：直接提取纯文本
        html_to_text(html)
    })
}

/// HTML 转纯文本。
/// 严格保留文本，丢弃所有 URL 与 Markdown 语法。链接的纯文本保留，URL 不附加。
fn html_to_text(html: &str) -> String {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);

    let body_sel = match Selector::parse("body") {
        Ok(s) => s,
        Err(_) => return strip_tags_fallback(html),
    };

    let mut out = String::new();

    for body in document.select(&body_sel) {
        collect_text(body, &mut out);
    }

    if out.is_empty() {
        return strip_tags_fallback(html);
    }

    out
}

/// 递归收集文本，跳过 script/style/noscript/template。
/// URL 在此层不做任何处理；纯文本模式不保留 URL。
fn collect_text(node: scraper::ElementRef, out: &mut String) {
    use scraper::ElementRef;

    for child in node.children() {
        match child.value() {
            scraper::node::Node::Text(t) => {
                let piece = t.trim();
                if !piece.is_empty() {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push(' ');
                    }
                    out.push_str(piece);
                }
            }
            scraper::node::Node::Element(el) => {
                let tag = el.name();
                if matches!(tag, "script" | "style" | "noscript" | "template") {
                    continue;
                }

                if let Some(child_el) = ElementRef::wrap(child) {
                    collect_text(child_el, out);

                    // 块级元素后追加换行，便于阅读结构
                    if is_block_tag(tag) && !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
            }
            _ => {}
        }
    }
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "ul"
            | "ol"
            | "li"
            | "table"
            | "tr"
            | "td"
            | "th"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "blockquote"
            | "pre"
            | "br"
    )
}

/// 最简降级：用正则粗暴去标签
fn strip_tags_fallback(html: &str) -> String {
    let mut s = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => s.push(c),
            _ => {}
        }
    }
    s
}

/// 规范化空白：裁剪首尾，压缩连续空行
fn normalize_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_empty = false;

    for line in s.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            if !prev_empty {
                result.push('\n');
                prev_empty = true;
            }
        } else {
            result.push_str(trimmed);
            result.push('\n');
            prev_empty = false;
        }
    }

    result.trim().to_string()
}

/// 在字符边界处截断，优先回退到最近的换行
fn truncate_at_boundary(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }

    // 尝试找到 max_chars 内最近的换行位置
    let mut byte_pos = 0;
    let mut char_count = 0;
    let mut last_newline_byte = 0;

    for ch in s.chars() {
        if char_count >= max_chars {
            break;
        }
        if ch == '\n' {
            last_newline_byte = byte_pos;
        }
        byte_pos += ch.len_utf8();
        char_count += 1;
    }

    // 优先在换行处截断（更自然）
    let cut_pos = if last_newline_byte > 0 {
        last_newline_byte
    } else {
        byte_pos
    };

    s[..cut_pos].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke_clean_html(args: Value) -> Result<ToolResult> {
        // 模拟同步调用
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async { clean_html(args) })
    }

    #[test]
    fn test_extract_article_content() {
        let html = r#"
        <html>
        <head><title>Test Article</title></head>
        <body>
            <nav><a href="/">Home</a></nav>
            <article>
                <h1>Main Title</h1>
                <p>This is the main content of the article with enough text to be extracted.</p>
                <p>Second paragraph with more content for readability detection.</p>
            </article>
            <footer>Copyright notice</footer>
            <script>alert('should be removed');</script>
        </body>
        </html>
        "#;

        let result = invoke_clean_html(json!({
            "html": html,
            "format": "markdown",
        }))
        .unwrap();

        let content = result.content["content"].as_str().unwrap();
        assert!(!content.contains("alert"), "script should be removed");
        assert!(!content.is_empty(), "content should not be empty");
    }

    #[test]
    fn test_fallback_to_full_page() {
        // 不含 article 标签，应降级为全页
        let html = r#"<html><body><div><p>Some simple text.</p></div></body></html>"#;

        let result = invoke_clean_html(json!({
            "html": html,
            "format": "text",
        }))
        .unwrap();

        let method = result.content["extraction_method"].as_str().unwrap();
        let content = result.content["content"].as_str().unwrap();
        assert!(content.contains("Some simple text"));
        assert_eq!(method, "full_page");
    }

    #[test]
    fn test_text_format_returns_plain_text() {
        let html = r#"<article><h1>Title</h1><p><a href="https://example.com">Link</a> text</p></article>"#;

        let result = invoke_clean_html(json!({
            "html": html,
            "format": "text",
        }))
        .unwrap();

        let format = result.content["format"].as_str().unwrap();
        let content = result.content["content"].as_str().unwrap();
        assert_eq!(format, "text");
        assert!(content.contains("Link"));
        assert!(!content.contains("https://example.com"));
        assert!(!content.contains("[Link]"));
    }

    #[test]
    fn test_markdown_format_preserves_urls() {
        let html = r#"<article><h1>Title</h1><p><a href="https://example.com">Example</a></p></article>"#;

        let result = invoke_clean_html(json!({
            "html": html,
            "format": "markdown",
        }))
        .unwrap();

        let format = result.content["format"].as_str().unwrap();
        assert_eq!(format, "markdown");
        let content = result.content["content"].as_str().unwrap();
        assert!(
            content.contains("https://example.com"),
            "markdown should preserve the link URL"
        );
    }

    #[test]
    fn test_chinese_utf8_safe_truncation() {
        let mut html = "<article>".to_string();
        for i in 0..500 {
            html.push_str(&format!("<p>这是中文段落编号{}，包含足够的字符用于测试截断功能。</p>", i));
        }
        html.push_str("</article>");

        let result = invoke_clean_html(json!({
            "html": html,
            "format": "text",
            "max_chars": 1000,
        }))
        .unwrap();

        let content = result.content["content"].as_str().unwrap();
        let truncated = result.content["truncated"].as_bool().unwrap();
        let returned = result.content["returned_chars"].as_u64().unwrap() as usize;

        assert!(truncated);
        assert!(returned <= 1000);
        // 验证不是乱码（每个中文字符是合法的）
        assert!(content.chars().all(|c| c != '\u{FFFD}'));
    }

    #[test]
    fn test_empty_html_returns_error() {
        let result = invoke_clean_html(json!({
            "html": "",
            "format": "text",
        }));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn test_max_chars_clamped() {
        let html = "<p>Hello world</p>";
        let result = invoke_clean_html(json!({
            "html": html,
            "format": "text",
            "max_chars": 500_000,
        }))
        .unwrap();

        // max_chars 应被 clamp 到 100_000
        let returned = result.content["returned_chars"].as_u64().unwrap();
        assert!(returned <= 100_000);
    }

    #[test]
    fn test_default_format_is_text() {
        let html = "<p>Hello</p>";
        let result = invoke_clean_html(json!({
            "html": html,
        }))
        .unwrap();

        let format = result.content["format"].as_str().unwrap();
        assert_eq!(format, "text");
    }

    #[test]
    fn test_registered_categories() {
        let provider = WebToolsProvider;
        let tools = provider.tools();
        assert_eq!(tools.len(), 1);
        let (tool, categories) = &tools[0];
        assert_eq!(tool.name, "builtin_clean_html");
        assert!(categories.contains(&ToolCategory::Browser));
        assert!(categories.contains(&ToolCategory::Text));
    }

    #[test]
    fn test_schema_contains_required_fields() {
        let provider = WebToolsProvider;
        let tools = provider.tools();
        let (tool, _) = &tools[0];
        let schema = &tool.input_schema;
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("html")));
    }
}
