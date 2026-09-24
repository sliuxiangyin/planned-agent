//! STATS Bento 块：执行统计。
//!
//! 指标来自执行器返回的 `PlanRunReport`（由执行服务的快照带上来：`RunSnapshot::report`）。
//! 没跑过（或模板未就绪）时，除 `Mode` 与步骤总数外一律显示占位符 `—`。
//!
//! 历史多次执行的对比属于执行记录落库（见 `docs/planned-agent/flexible-executor.md` 阶段 6）。

use dioxus::prelude::*;
use planned_agent::flexible::PlanRunReport;

#[component]
pub fn StatsView(
    plan_mode_label: String,
    total_steps: usize,
    hint: Option<String>,
    // 最近一次执行的报告；`None` = 本会话还没跑过
    report: Option<PlanRunReport>,
) -> Element {
    // 先把要显示的文本算好，避免 rsx 里塞三元表达式。
    let exec_time = report
        .as_ref()
        .map(|r| format_duration(r.total_duration_ms))
        .unwrap_or_else(|| "—".to_string());
    let tokens = report
        .as_ref()
        .map(|r| format_tokens(r.total_tokens()))
        .unwrap_or_else(|| "—".to_string());
    let tools_called = report
        .as_ref()
        .map(|r| r.tool_calls.to_string())
        .unwrap_or_else(|| "—".to_string());
    let steps_done = report
        .as_ref()
        .map(|r| format!("{}/{}", r.steps_done(), total_steps))
        .unwrap_or_else(|| format!("0/{total_steps}"));
    let errors = report
        .as_ref()
        .map(|r| r.errors().to_string())
        .unwrap_or_else(|| "—".to_string());

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
                        span { class: "plan-stats__value plan-stats__value--highlight", "{exec_time}" }
                    }
                    div { class: "plan-stats__row",
                        span { class: "plan-stats__label", "Tokens" }
                        span { class: "plan-stats__value", "{tokens}" }
                    }
                    div { class: "plan-stats__row",
                        span { class: "plan-stats__label", "Tools called" }
                        span { class: "plan-stats__value", "{tools_called}" }
                    }
                    div { class: "plan-stats__row",
                        span { class: "plan-stats__label", "Steps done" }
                        span { class: "plan-stats__value plan-stats__value--success", "{steps_done}" }
                    }
                    div { class: "plan-stats__row",
                        span { class: "plan-stats__label", "Errors" }
                        span { class: "plan-stats__value", "{errors}" }
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

/// 毫秒 → `345ms` / `1.2s`。
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

/// token 数 → `842` / `1.2k`。
fn format_tokens(tokens: u32) -> String {
    if tokens < 1000 {
        tokens.to_string()
    } else {
        format!("{:.1}k", tokens as f64 / 1000.0)
    }
}
