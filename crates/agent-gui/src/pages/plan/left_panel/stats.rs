//! STATS Bento 块：执行统计。
//!
//! 执行指标（耗时 / token / 工具调用数 / 错误数）由执行器的 `PlanRunReport`
//! 提供，落库属于执行记录（见 `docs/planned-agent/flexible-executor.md` 阶段 6）；
//! 在此之前，除 `Mode` 与步骤总数外一律显示占位符 `—`。

use dioxus::prelude::*;

#[component]
pub fn StatsView(plan_mode_label: String, total_steps: usize, hint: Option<String>) -> Element {
    rsx! {
        div { class: "plan-bento-block",
            div { class: "plan-bento-block__header",
                span { class: "plan-bento-block__header-emoji", "⚡" }
                span { class: "plan-bento-block__header-label", "STATS" }
            }
            div { class: "plan-bento-block__body",
                if let Some(hint) = hint {
                    div { class: "plan-bento-empty", "{hint}" }
                } else {
                    div { class: "plan-stats__row",
                        span { class: "plan-stats__label", "Exec time" }
                        span { class: "plan-stats__value plan-stats__value--highlight", "—" }
                    }
                    div { class: "plan-stats__row",
                        span { class: "plan-stats__label", "Tokens" }
                        span { class: "plan-stats__value", "—" }
                    }
                    div { class: "plan-stats__row",
                        span { class: "plan-stats__label", "Tools called" }
                        span { class: "plan-stats__value", "—" }
                    }
                    div { class: "plan-stats__row",
                        span { class: "plan-stats__label", "Steps done" }
                        span { class: "plan-stats__value plan-stats__value--success", "0/{total_steps}" }
                    }
                    div { class: "plan-stats__row",
                        span { class: "plan-stats__label", "Errors" }
                        span { class: "plan-stats__value", "—" }
                    }
                }
                // Mode 来自 plan 本身（与会话模板无关），不随模板空态隐藏。
                div { class: "plan-stats__row",
                    span { class: "plan-stats__label", "Mode" }
                    span { class: "plan-stats__value", "{plan_mode_label}" }
                }
            }
        }
    }
}
