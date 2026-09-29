//! STATS Bento 块：执行统计。
//!
//! 数据源是**执行快照**（`RunSnapshot`），不是终态的 `PlanRunReport`。
//! 报告只在 `RunFinished` 时才被填进快照（`state.rs`），用它会让整块 STATS
//! 全程显示 `—`，直到执行结束才「跳」出来。快照则在每个事件后更新，所以
//! 执行中就能看到 Exec time / Tokens / Tools / Steps done / Errors 在涨。
//!
//! 一处例外：`Tokens` 按步结算（`StepFinished` 时才有值），正在跑的那一步
//! 还没计入 —— 它不是逐 token 实时，但也不像以前那样全程空着。
//!
//! 没跑过（或模板未就绪）时，除 `Mode` 与步骤总数外一律显示占位符 `—`。
//!
//! 历史多次执行的对比属于执行记录落库（见 `docs/planned-agent/flexible-executor.md` 阶段 6）。

use dioxus::prelude::*;
use planned_agent::flexible::run_service::{RunSnapshot, StepPhase};

#[component]
pub fn StatsView(
    plan_mode_label: String,
    total_steps: usize,
    hint: Option<String>,
    /// 该会话当前（或最近一次）的执行快照；`None` = 本会话还没跑过。
    snapshot: Option<RunSnapshot>,
) -> Element {
    // 先把要显示的文本算好，避免 rsx 里塞三元表达式。
    let exec_time = snapshot
        .as_ref()
        .map(format_exec_time)
        .unwrap_or_else(|| "—".to_string());
    let tokens = snapshot
        .as_ref()
        .map(|s| format_tokens(steps_token_sum(s)))
        .unwrap_or_else(|| "—".to_string());
    let tools_called = snapshot
        .as_ref()
        .map(|s| {
            s.steps
                .iter()
                .map(|step| step.tool_calls)
                .sum::<usize>()
                .to_string()
        })
        .unwrap_or_else(|| "—".to_string());
    // 与上行互补：上行答「调了几次」，这行答「调了什么」。
    let tools_used = snapshot
        .as_ref()
        .map(format_tools_used)
        .unwrap_or_else(|| "—".to_string());
    let steps_done = snapshot
        .as_ref()
        .map(|s| format!("{}/{total_steps}", count_phase(s, StepPhase::Done)))
        .unwrap_or_else(|| format!("0/{total_steps}"));
    let errors = snapshot
        .as_ref()
        .map(|s| count_phase(s, StepPhase::Failed).to_string())
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

/// 执行耗时：运行中显示「到现在为止」，已结束显示起止之差。
///
/// ⚠️ 只随**事件**刷新 —— 一次 LLM 调用期间没有事件，数字会停住不涨
/// （不会算错，只是暂停）。要严格逐秒跳动得引入定时器，代价不成比例。
fn format_exec_time(snapshot: &RunSnapshot) -> String {
    let end = snapshot.finished_at_ms.unwrap_or_else(now_ms);
    format_duration(end.saturating_sub(snapshot.started_at_ms))
}

/// 已完成步骤的 token 之和（`StepFinished` 才结算，正在跑的那步还没计入）。
fn steps_token_sum(snapshot: &RunSnapshot) -> u32 {
    snapshot
        .steps
        .iter()
        .map(|step| step.prompt_tokens + step.completion_tokens)
        .sum()
}

/// 处于某相位的步骤数。
fn count_phase(snapshot: &RunSnapshot, phase: StepPhase) -> usize {
    snapshot
        .steps
        .iter()
        .filter(|step| step.phase == phase)
        .count()
}

/// 本次执行**用到过**的工具：去重后按首次出现顺序，带次数（`read_file ×2`）。
///
/// 一步都没调工具时返回 `—`。
fn format_tools_used(snapshot: &RunSnapshot) -> String {
    format_tool_names(
        snapshot
            .steps
            .iter()
            .flat_map(|step| step.tool_sequence.iter())
            .map(|call| call.tool.as_str()),
    )
}

/// 工具名序列 → `read_file ×2、write_file ×1`。
///
/// 去重、按**首次出现顺序**（不排序 —— 顺序本身反映流程：先读后写），
/// 最多列 3 个，其余的折成「等 N 个」（工具数本身就是「该不该收窄工具表」的信号）。
fn format_tool_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    // `(工具名, 次数)`，按首次出现顺序。
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for name in names {
        match counts.iter_mut().find(|(seen, _)| *seen == name) {
            Some((_, count)) => *count += 1,
            None => counts.push((name, 1)),
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

/// 当前 Unix 毫秒（与内核 `now_ms` 同口径）。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_deduped_in_first_seen_order() {
        assert_eq!(
            format_tool_names(["read_file", "write_file", "read_file"].into_iter()),
            "read_file ×2、write_file ×1"
        );
    }

    #[test]
    fn no_tool_calls_shows_placeholder() {
        assert_eq!(format_tool_names(std::iter::empty()), "—");
    }

    #[test]
    fn extra_tools_are_folded_into_a_count() {
        let names = ["a", "b", "c", "d", "e"];
        assert_eq!(format_tool_names(names.into_iter()), "a ×1、b ×1、c ×1 等 5 个");
    }
}
