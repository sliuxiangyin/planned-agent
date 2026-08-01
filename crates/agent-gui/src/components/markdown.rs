//! Markdown 渲染组件
//!
//! 把 Markdown 文本解析为 HTML，再经 [`ammonia`] 清理（白名单 sanitize），最后通过
//! Dioxus 的 `dangerous_inner_html` 渲染到 DOM。
//!
//! ## 设计要点
//!
//! - **解析**：[`pulldown_cmark`]（CommonMark + 表格/删除线/任务列表扩展），流式友好
//!   （解析速度 ~10 MB/s，KB 级文本每次解析亚毫秒，可每 chunk 调用一次）
//! - **清理**：[`ammonia`] 白名单过滤，去掉 `<script>` / `<style>` / `onclick=` 等
//!   危险内容，避免 LLM 输出未闭合标签或恶意 HTML 触发 XSS
//! - **样式**：本组件**不带**样式表，markdown 元素的视觉由 `plan.css` 控制（与
//!   `.chat-message` 容器配合，详见 `assets/plan.css` 的 `.chat-message .markdown *` 段）
//!
//! ## Props
//!
//! - `text`：原始 Markdown 字符串
//! - `class`：可选，附加到外层 `<div class="markdown">` 的额外 class（默认空）

use dioxus::prelude::*;

/// Markdown 渲染组件的属性
#[derive(Props, Clone, PartialEq)]
pub struct MarkdownProps {
    /// 原始 Markdown 文本
    pub text: String,
    /// 外层 div 的额外 class（与 "markdown" 并存）
    #[props(default)]
    pub class: Option<String>,
}

/// Markdown 组件
#[component]
pub fn Markdown(props: MarkdownProps) -> Element {
    let html = render_markdown_to_safe_html(&props.text);
    let class = match &props.class {
        Some(extra) if !extra.is_empty() => format!("markdown {}", extra),
        _ => "markdown".to_string(),
    };
    rsx! {
        div { class: "{class}", dangerous_inner_html: "{html}" }
    }
}

/// Markdown → HTML → sanitize（公开以便其他模块复用，如截图/导出场景）
///
/// 启用扩展：表格（GFM）/ 删除线 / 任务列表 / 脚注 / 智能标点。
pub fn render_markdown_to_safe_html(text: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);

    let parser = Parser::new_ext(text, options);
    let mut raw_html = String::new();
    html::push_html(&mut raw_html, parser);

    // ammonia 白名单清理：保留语义标签，去掉 <script>/<style>/on* 属性/javascript: URL
    ammonia::clean(&raw_html)
}
