//! STATS Bento 块：执行统计。
//!
//! 展示执行耗时、Token 用量、工具调用数、完成步骤、错误数与模式。
//! 当前为静态 mock，后续接入真实执行指标。

use dioxus::prelude::*;

#[component]
pub fn StatsView(plan_mode_label: String) -> Element {
    rsx! {
        div { class: "plan-bento-block",
            div { class: "plan-bento-block__header",
                span { class: "plan-bento-block__header-emoji", "⚡" }
                span { class: "plan-bento-block__header-label", "STATS" }
            }
            div { class: "plan-bento-block__body",
                div { class: "plan-stats__row",
                    span { class: "plan-stats__label", "Exec time" }
                    span { class: "plan-stats__value plan-stats__value--highlight", "6.4s" }
                }
                div { class: "plan-stats__row",
                    span { class: "plan-stats__label", "Tokens" }
                    span { class: "plan-stats__value", "1.2k / 8.0k" }
                }
                div { class: "plan-stats__row",
                    span { class: "plan-stats__label", "Tools called" }
                    span { class: "plan-stats__value", "5" }
                }
                div { class: "plan-stats__row",
                    span { class: "plan-stats__label", "Steps done" }
                    span { class: "plan-stats__value plan-stats__value--success", "2/4" }
                }
                div { class: "plan-stats__row",
                    span { class: "plan-stats__label", "Errors" }
                    span { class: "plan-stats__value", "0" }
                }
                div { class: "plan-stats__row",
                    span { class: "plan-stats__label", "Mode" }
                    span { class: "plan-stats__value", "{plan_mode_label}" }
                }
            }
        }
    }
}
