//! ReasoningView 组件：渲染 Assistant 消息的「深度思考」折叠面板。
//!
//! - 默认折叠；点击头部展开/收起
//! - 流式中（`is_streaming = true`）：呼吸/脉冲高亮动画 + "思考中…"
//! - 流式结束：动画停止 + "深度思考 · N 字"
//! - 展开后：内嵌 Markdown 渲染全文
//!
//! 折叠状态由组件自身 `use_signal` 管理（无外部状态依赖）；
//! 调用方只需传入 `text` 与 `is_streaming` 两个 prop。
//!
//! `text` 为空时整个组件不渲染（直接返回空 rsx）。

use dioxus::prelude::*;

use crate::components::markdown::Markdown;

#[css_module("/src/pages/plan/components/reasoning_view/style.css")]
struct Styles;

/// Assistant 消息的「深度思考」折叠面板。
///
/// # Props
/// - `text` — 累积的推理内容；空串时不渲染
/// - `is_streaming` — 是否仍在流式接收；true 时启用脉冲动画
#[component]
pub fn ReasoningView(text: String, is_streaming: bool) -> Element {
    // 折叠状态：组件局部 state，无需外部管理
    let mut open = use_signal(|| false);
    let is_open = *open.read();

    // text 为空 → 整块不渲染
    if text.is_empty() {
        return rsx! {};
    }

    let char_count = text.chars().count();
    let status = if is_streaming {
        "思考中…".to_string()
    } else {
        format!("深度思考 · {char_count} 字")
    };

    rsx! {
        div {
            class: Styles::reasoning_view,
            "data-active": if is_streaming { "true" } else { "false" },
            "data-open": if is_open { "true" } else { "false" },
            div {
                class: Styles::reasoning_view__header,
                onclick: move |_| open.toggle(),
                // 折叠箭头（SVG，旋转动画）
                svg {
                    class: Styles::reasoning_view__chevron,
                    view_box: "0 0 16 16",
                    width: "12",
                    height: "12",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "1.75",
                    stroke_linecap: "round",
                    stroke_linejoin: "round",
                    path { d: "M 4 6 L 8 10 L 12 6" }
                }
                span {
                    class: Styles::reasoning_view__status,
                    "{status}"
                }
            }
            if is_open {
                div {
                    class: Styles::reasoning_view__body,
                    Markdown { text: text.clone() }
                }
            }
        }
    }
}