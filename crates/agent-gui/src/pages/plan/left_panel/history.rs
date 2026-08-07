//! HISTORY Bento 块：历史版本列表。
//!
//! 展示历次执行版本（v4 当前 / v3 完成 / v2 取消 / v1 失败）。
//! 当前为静态 mock，后续接入 `plans_flexible` 表历史快照。

use dioxus::prelude::*;

#[component]
pub fn HistoryView() -> Element {
    rsx! {
        div { class: "plan-bento-block",
            div { class: "plan-bento-block__header",
                span { class: "plan-bento-block__header-emoji", "📜" }
                span { class: "plan-bento-block__header-label", "HISTORY" }
                span { class: "plan-bento-block__header-spacer" }
                button {
                    class: "plan-bento-header-btn",
                    title: "加载所选版本",
                    svg {
                        xmlns: "http://www.w3.org/2000/svg",
                        width: "13",
                        height: "13",
                        view_box: "0 0 24 24",
                        fill: "none",
                        stroke: "currentColor",
                        stroke_width: "2",
                        stroke_linecap: "round",
                        stroke_linejoin: "round",
                        path { d: "M21 12a9 9 0 1 1-6.219-8.56" }
                        path { d: "M21 3v5h-5" }
                    }
                }
            }
            div { class: "plan-bento-block__body",
                div { class: "plan-history__list",
                    // v4 — 当前
                    div {
                        class: "plan-history__item plan-history__item--current",
                        span { class: "plan-history__version", "v4" }
                        span { class: "plan-history__time", "08-06 10:30" }
                        span { class: "plan-history__separator", "·" }
                        span { class: "plan-history__status plan-history__status--ok", "✓ 完成" }
                        span { class: "plan-history__separator", "·" }
                        span { class: "plan-history__meta", "4/4 · 12.3s · 7 tk" }
                    }
                    // v3
                    div {
                        class: "plan-history__item",
                        span { class: "plan-history__version", "v3" }
                        span { class: "plan-history__time", "08-05 14:20" }
                        span { class: "plan-history__separator", "·" }
                        span { class: "plan-history__status plan-history__status--ok", "✓ 完成" }
                        span { class: "plan-history__separator", "·" }
                        span { class: "plan-history__meta", "3/4 · 8.7s · 5 tk" }
                    }
                    // v2
                    div {
                        class: "plan-history__item plan-history__item--failed",
                        span { class: "plan-history__version", "v2" }
                        span { class: "plan-history__time", "08-05 09:00" }
                        span { class: "plan-history__separator", "·" }
                        span { class: "plan-history__status plan-history__status--fail", "✗ 取消" }
                        span { class: "plan-history__separator", "·" }
                        span { class: "plan-history__meta", "1/4 · — · 2 tk" }
                    }
                    // v1
                    div {
                        class: "plan-history__item plan-history__item--failed",
                        span { class: "plan-history__version", "v1" }
                        span { class: "plan-history__time", "08-04 18:30" }
                        span { class: "plan-history__separator", "·" }
                        span { class: "plan-history__status plan-history__status--fail", "✗ 失败" }
                        span { class: "plan-history__separator", "·" }
                        span { class: "plan-history__meta", "0/4 · — · 1 tk" }
                    }
                }
            }
        }
    }
}
