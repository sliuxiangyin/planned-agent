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
    // 与上行互补：上行答「调了几次」，这行答「调了什么」。
    let tools_used = report
        .as_ref()
        .map(format_tools_used)
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
                        span { class: "plan-stats__label", "Tools used" }
                        span { class: "plan-stats__value", "{tools_used}" }
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

/// 本次执行**用到过**的工具：去重后按首次出现顺序，带次数（`read_file ×2`）。
///
/// 特意不截到「只看前几个」而不报剩余 —— 工具数量本身就是「该不该收窄工具表」的信号。
/// 一步都没调工具时返回 `—`。
fn format_tools_used(report: &PlanRunReport) -> String {
    // `(工具名, 次数)`，按首次出现顺序。
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for step in &report.steps {
        for call in &step.tool_sequence {
            match counts
                .iter_mut()
                .find(|(name, _)| *name == call.tool.as_str())
            {
                Some((_, count)) => *count += 1,
                None => counts.push((call.tool.as_str(), 1)),
            }
        }
    }
    if counts.is_empty() {
        return "—".to_string();
    }

    const MAX_SHOWN: usize = 3;
    let mut text = counts
        .iter()
        .take(MAX_SHOWN)
        .map(|(name, count)| format!("{name} ×{count}"))
        .collect::<Vec<_>>()
        .join("、");
    if counts.len() > MAX_SHOWN {
        text.push_str(&format!(" 等 {} 个", counts.len()));
    }
    text
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
