//! 启发式结构索引生成。
//!
//! 根据文本内容类型，自动提取章节结构，生成 `Vec<Section>`。
//! 零额外 token 消耗（纯启发式，不调 LLM）。

use super::chunk_view::Section;

/// 内容类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Html,
    Markdown,
    Json,
    PlainText,
}

// ── 入口 ──────────────────────────────────────────────

/// 自动检测内容类型并生成结构索引。
pub fn build_index(text: &str) -> Vec<Section> {
    let ct = detect_content_type(text);
    build_index_for(text, ct)
}

/// 按已知类型构建索引。
pub fn build_index_for(text: &str, ct: ContentType) -> Vec<Section> {
    match ct {
        ContentType::Html => index_html(text),
        ContentType::Markdown => index_markdown(text),
        ContentType::Json => index_json(text),
        ContentType::PlainText => index_plain_text(text),
    }
}

/// 自动检测内容类型。
pub fn detect_content_type(text: &str) -> ContentType {
    let trimmed = text.trim_start();

    if trimmed.len() < 16 {
        return ContentType::PlainText;
    }

    // HTML 检测
    if looks_like_html(trimmed) {
        return ContentType::Html;
    }

    // JSON 检测
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<Value>(trimmed).is_ok()
    {
        return ContentType::Json;
    }

    // Markdown 检测：含有 # 标题行
    if trimmed.lines().any(|l| l.starts_with('#')) {
        return ContentType::Markdown;
    }

    ContentType::PlainText
}

// ── HTML 索引 ─────────────────────────────────────────

fn index_html(text: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut byte_offset = 0usize;

    // 用简单状态机提取 <h1>~<h6> 标签
    let lower = text.to_lowercase();
    let mut rest = &lower[..];

    while !rest.is_empty() {
        if let Some(tag_start) = rest.find("<h") {
            let tag_rest = &rest[tag_start..];
            // 检查是否是 <h1> ~ <h6>
            if tag_rest.len() >= 4
                && tag_rest.as_bytes()[2].is_ascii_digit()
                && (tag_rest.as_bytes()[3] == b'>' || tag_rest.as_bytes()[3] == b' ')
            {
                let abs_start = byte_offset + tag_start;

                // 找到 > 作为标题文本开始
                if let Some(gt_pos) = tag_rest.find('>') {
                    let content_start = tag_start + gt_pos + 1;
                    let _content_rest = &rest[content_start..];

                    // 找到对应的 </hN>
                    let close_tag = format!("</h{}", tag_rest.as_bytes()[2] as char);
                    if let Some(close_pos) = lower[byte_offset + content_start..].find(&close_tag)
                    {
                        let title_raw = &text[byte_offset + content_start
                            ..byte_offset + content_start + close_pos];
                        let title = strip_tags(title_raw).trim().to_string();
                        let abs_end = byte_offset + content_start + close_pos + close_tag.len();

                        if !title.is_empty() {
                            sections.push(Section {
                                title,
                                start_byte: abs_start,
                                end_byte: abs_end,
                            });
                        }
                    }
                }
            }
        }

        // 前进一个字符继续搜索
        if let Some(ch) = rest.chars().next() {
            byte_offset += ch.len_utf8();
            rest = &rest[ch.len_utf8()..];
        } else {
            break;
        }
    }

    if sections.is_empty() {
        // 回退：按文本分块作为索引
        return index_plain_text(text);
    }

    sections
}

/// 简单 strip_tags，用于提取标题纯文本。
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

// ── Markdown 索引 ─────────────────────────────────────

fn index_markdown(text: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut byte_offset = 0usize;

    for line in text.lines() {
        let line_bytes = line.len();
        if let Some(stripped) = line.strip_prefix('#') {
            let level = stripped.chars().take_while(|c| *c == '#').count();
            if level <= 6 && stripped.len() > level {
                let after = &stripped[level..];
                let title = if after.starts_with(' ') {
                    after[1..].trim()
                } else {
                    after.trim()
                };
                if !title.is_empty() {
                    sections.push(Section {
                        title: title.to_string(),
                        start_byte: byte_offset,
                        end_byte: byte_offset + line_bytes,
                    });
                }
            }
        }
        byte_offset += line_bytes + 1; // +1 for newline
    }

    if sections.is_empty() {
        return index_plain_text(text);
    }

    // 补全 end_byte：每个 section 的结束为下一个的开始
    for i in 0..sections.len() {
        let end = if i + 1 < sections.len() {
            sections[i + 1].start_byte
        } else {
            text.len()
        };
        sections[i].end_byte = end;
    }

    sections
}

// ── JSON 索引 ─────────────────────────────────────────

use serde_json::Value;

fn index_json(text: &str) -> Vec<Section> {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return index_plain_text(text),
    };

    let mut sections = Vec::new();

    match &parsed {
        Value::Object(map) => {
            // 对每个顶层 key 生成一条索引
            let mut offset = 0usize;
            for (key, val) in map {
                let val_str = serde_json::to_string(val).unwrap_or_default();
                let desc = match val {
                    Value::Array(arr) => format!("{}({} items)", key, arr.len()),
                    Value::Object(_) => format!("{} (object)", key),
                    Value::String(s) => {
                        let preview: String = s.chars().take(60).collect();
                        if s.len() > 60 {
                            format!("{} \"{}\"...", key, preview)
                        } else {
                            format!("{} \"{}\"", key, s)
                        }
                    }
                    _ => format!("{} ({})", key, val_str),
                };
                sections.push(Section {
                    title: desc,
                    start_byte: offset,
                    end_byte: offset + val_str.len(),
                });
                offset += val_str.len();
            }
        }
        Value::Array(arr) => {
            sections.push(Section {
                title: format!("Array ({} items)", arr.len()),
                start_byte: 0,
                end_byte: text.len(),
            });
        }
        _ => {
            return index_plain_text(text);
        }
    }

    sections
}

// ── 纯文本索引 ────────────────────────────────────────

fn index_plain_text(text: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut byte_offset = 0usize;

    // 按双换行分段
    for block in text.split("\n\n") {
        let block_trimmed = block.trim();
        if block_trimmed.is_empty() {
            byte_offset += block.len() + 2;
            continue;
        }

        // 取首行作为标题（限制长度）
        let first_line = block_trimmed.lines().next().unwrap_or("");
        let title: String = if first_line.len() > 80 {
            first_line.chars().take(77).chain("...".chars()).collect()
        } else {
            first_line.to_string()
        };

        sections.push(Section {
            title,
            start_byte: byte_offset,
            end_byte: byte_offset + block_trimmed.len(),
        });

        byte_offset += block.len() + 2;
    }

    if sections.is_empty() {
        // 极端情况：整个文本作为单一段落
        sections.push(Section {
            title: "全文".to_string(),
            start_byte: 0,
            end_byte: text.len(),
        });
    }

    sections
}

// ── HTML 检测 ─────────────────────────────────────────

fn looks_like_html(s: &str) -> bool {
    let trimmed = s.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with("<!doctype") || trimmed.starts_with("<!DOCTYPE") {
        return true;
    }
    if trimmed.starts_with("<html") || trimmed.starts_with("<HTML") {
        return true;
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
    const TAGS: &[&str] = &[
        "<div", "<p ", "<P ", "<p>", "<P>", "<a ", "<span", "<article",
        "<section", "<header", "<footer", "<nav", "<table", "<ul", "<ol", "<li",
        "<h1", "<h2", "<h3", "<h4", "<h5", "<h6", "<img", "<body", "<head",
    ];
    TAGS.iter().any(|t| head.contains(t))
}

// ── 测试 ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_html() {
        assert_eq!(
            detect_content_type("<!doctype html><html><body>hi</body></html>"),
            ContentType::Html
        );
    }

    #[test]
    fn detect_markdown() {
        let md = "# Title\n\n## Section 1\n\nSome text\n\n### Sub";
        assert_eq!(detect_content_type(md), ContentType::Markdown);
    }

    #[test]
    fn detect_json() {
        assert_eq!(
            detect_content_type(r#"{"key": "value", "arr": [1,2,3]}"#),
            ContentType::Json
        );
    }

    #[test]
    fn detect_plain() {
        assert_eq!(
            detect_content_type("just a plain sentence without markup"),
            ContentType::PlainText
        );
    }

    #[test]
    fn markdown_index_generates_sections() {
        let md = "# Title\n\ntext\n\n## Section 1\n\ncontent\n\n## Section 2\n\nmore";
        let sections = index_markdown(md);
        assert!(!sections.is_empty());
        assert!(sections.iter().any(|s| s.title == "Title"));
        assert!(sections.iter().any(|s| s.title == "Section 1"));
    }

    #[test]
    fn json_index_lists_keys() {
        let json = r#"{"name": "test", "items": [1,2,3], "meta": {"k": "v"}}"#;
        let sections = index_json(json);
        assert!(!sections.is_empty());
        assert!(sections.iter().any(|s| s.title.contains("name")));
    }

    #[test]
    fn plain_text_index_uses_first_lines() {
        let text = "First block line 1\nline 2\n\nSecond block\ncontent\n\nThird";
        let sections = build_index(text);
        assert!(!sections.is_empty());
        assert!(sections.iter().any(|s| s.title.contains("First block")));
    }
}
