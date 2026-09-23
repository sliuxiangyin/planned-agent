//! HISTORY Bento 块：历史执行记录。
//!
//! 执行历史落库见 `docs/planned-agent/flexible-executor.md` 阶段 6；
//! 在此之前恒为空态（模板未就绪时显示其原因，其余显示「暂无执行记录」）。

use dioxus::prelude::*;

#[component]
pub fn HistoryView(hint: Option<String>) -> Element {
    let text = hint.unwrap_or_else(|| "暂无执行记录".to_string());

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
                div { class: "plan-bento-empty", "{text}" }
            }
        }
    }
}
