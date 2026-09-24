//! PIPELINE Bento 块：执行时间线。
//!
//! 步骤文本是**已按当前参数值展开**的（展开在 `PlanLeftPanel::render_steps`），
//! 所以 PARAMS 一改参数，这里立刻跟着变；未填的参数保留 `${name}` 原样并给出提示。
//!
//! 每步的相位（待执行 / 执行中 / 成功 / 失败 / 跳过）由执行服务的快照（`RunSnapshot::phase_of`）
//! 驱动 —— 执行在常驻服务里跑，本组件卸载并不影响它。

use dioxus::prelude::*;

use planned_agent::flexible::run_service::{StepPhase, StepTrackLine};

use super::left_panel::{missing_label, RenderedStep};

#[component]
pub fn PipelineView(
    steps: Vec<RenderedStep>,
    hint: Option<String>,
    // 是否正在执行（决定停止按钮是否可用）
    is_running: bool,
    // 是否可执行：模板就绪 + 无未填参数 + 不在执行中
    can_run: bool,
    // 首个错误信息（单步 intent 展开失败或整次失败）
    error: Option<String>,
    on_run: EventHandler<MouseEvent>,
    on_stop: EventHandler<MouseEvent>,
) -> Element {
    let total = steps.len();
    let progressed = steps
        .iter()
        .filter(|step| matches!(step.phase, StepPhase::Done | StepPhase::Failed))
        .count();
    let empty_hint = hint.or_else(|| steps.is_empty().then(|| "该模板未定义步骤".to_string()));

    rsx! {
        div { class: "plan-bento-block",
            div { class: "plan-bento-block__header",
                span { class: "plan-bento-block__header-emoji", "🎯" }
                span { class: "plan-bento-block__header-label", "PIPELINE" }
                span { class: "plan-bento-block__header-spacer" }
                // 历史按钮（执行记录落库后启用，见阶段 6）
                button {
                    class: "plan-bento-header-btn",
                    title: "历史版本",
                    svg {
                        xmlns: "http://www.w3.org/2000/svg",
                        width: "14",
                        height: "14",
                        view_box: "0 0 24 24",
                        fill: "none",
                        stroke: "currentColor",
                        stroke_width: "2",
                        stroke_linecap: "round",
                        stroke_linejoin: "round",
                        circle { cx: "12", cy: "12", r: "10" }
                        polyline { points: "12 6 12 12 16 14" }
                    }
                }
                // 执行按钮：参数没填全 / 模板未就绪 / 已在执行 → 禁用
                button {
                    class: "plan-bento-header-btn",
                    title: if can_run { "执行计划" } else { "需模板就绪且参数填写完整" },
                    disabled: !can_run,
                    onclick: move |event| on_run.call(event),
                    svg {
                        xmlns: "http://www.w3.org/2000/svg",
                        width: "14",
                        height: "14",
                        view_box: "0 0 16 16",
                        fill: "currentColor",
                        path { d: "M 4 2.5 L 13 8 L 4 13.5 Z" }
                    }
                }
                // 停止按钮：唯一的中断途径
                button {
                    class: "plan-bento-header-btn",
                    title: "停止执行",
                    disabled: !is_running,
                    onclick: move |event| on_stop.call(event),
                    svg {
                        xmlns: "http://www.w3.org/2000/svg",
                        width: "14",
                        height: "14",
                        view_box: "0 0 16 16",
                        fill: "currentColor",
                        rect { x: "3", y: "2.5", width: "3.5", height: "11" }
                        rect { x: "9.5", y: "2.5", width: "3.5", height: "11" }
                    }
                }
            }

            div { class: "plan-bento-block__body",
                if let Some(hint) = empty_hint {
                    div { class: "plan-bento-empty", "{hint}" }
                } else {
                    div { class: "plan-pipeline__timeline",
                        for (index, step) in steps.iter().enumerate() {
                            div {
                                class: "plan-pipeline__step plan-pipeline__step--{step.phase.css_suffix()}",
                                key: "{index}",
                                div { class: "plan-pipeline__step-rail",
                                    div { class: "plan-pipeline__step-dot",
                                        svg {
                                            class: "plan-pipeline-node--{step.phase.node_suffix()}",
                                            xmlns: "http://www.w3.org/2000/svg",
                                            view_box: "0 0 16 16",
                                            width: "14",
                                            height: "14",
                                            fill: "none",
                                            stroke: "currentColor",
                                            stroke_width: "1.5",
                                            circle { cx: "8", cy: "8", r: "6" }
                                        }
                                    }
                                    div { class: "plan-pipeline__step-line plan-pipeline__step-line--{step.phase.line_suffix()}" }
                                }
                                div { class: "plan-pipeline__step-body",
                                    div { class: "plan-pipeline__step-header",
                                        span { class: "plan-pipeline__step-index", "S{index + 1}" }
                                        span { class: "plan-pipeline__step-title", "{step.intent}" }
                                        span { class: "plan-pipeline__step-meta", "{step.reference}" }
                                    }
                                    div { class: "plan-pipeline__step-detail", "预期输出: {step.expected_output}" }
                                    if !step.missing.is_empty() {
                                        div { class: "plan-pipeline__step-detail",
                                            "⚠ 未填参数: {missing_label(&step.missing)}"
                                        }
                                    }
                                    // 执行轨迹：跑过的步骤留下「怎么走过来的」，正在跑的实时长；
                                    // 未执行的步骤不占版面。
                                    if !step.track.is_empty() || step.phase == StepPhase::Running {
                                        ThinkBox { phase: step.phase, track: step.track.clone() }
                                    }
                                    // 本步产出：执行过才有。默认收起 —— 结果可能很长，
                                    // 与 think box 一样，把「要不要看细节」交给用户。
                                    if let Some(output) = step.output.clone() {
                                        StepOutput {
                                            truncated: step.output_truncated,
                                            output: output,
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if let Some(error) = error {
                        div { class: "plan-pipeline__error", "⚠ {error}" }
                    }

                    // 底部状态栏：详细指标在 STATS，这里只给进度与执行状态。
                    div { class: "plan-pipeline__statusbar",
                        span { class: "plan-pipeline__statusbar-item", "{progressed}/{total} steps" }
                        span { class: "plan-pipeline__statusbar-item", "⏱ " }
                        span { class: "plan-pipeline__statusbar-item", "🔧 " }
                        span { class: "plan-pipeline__statusbar-item", "📝 " }
                    }
                }
            }
        }
    }
}

/// 单步产出（默认收起）：执行过后才有。
///
/// 与 think box 分开：think box 是「过程」，这里是「这一步交出了什么」。
/// 超长时执行器只留前若干字符（`output_truncated`），这里如实标注，不假装完整。
#[component]
fn StepOutput(truncated: bool, output: String) -> Element {
    rsx! {
        details { class: "plan-step-output",
            summary { class: "plan-step-output__summary",
                if truncated {
                    "本步产出（超长已截断）"
                } else {
                    "本步产出"
                }
            }
            pre { class: "plan-step-output__text", "{output}" }
        }
    }
}

/// 单步的思考与动作轨迹（think box）。
///
/// `$` 行 = 该步发起的一次工具调用（工具名 + 关键入参）；
/// `>` 行 = 该轮 LLM 的思考文本。
/// 轨迹由执行服务**累积**推送（`StepSnapshot::track`），故执行结束后仍能回看这一步的来路。
#[component]
fn ThinkBox(phase: StepPhase, track: Vec<StepTrackLine>) -> Element {
    rsx! {
        div { class: "plan-think-box",
            if track.is_empty() {
                // 只有「正在跑但还没产出第一行」会走到这里：未执行的步骤外层不渲染本块。
                span { class: "plan-think-box__line plan-think-box__empty", "正在思考…" }
            }
            for (index, line) in track.iter().enumerate() {
                match line {
                    StepTrackLine::Tool { tool, args, .. } => rsx! {
                        span { key: "{index}", class: "plan-think-box__line",
                            span { class: "plan-think-box__prompt", "$ " }
                            "{tool} {args}"
                        }
                    },
                    StepTrackLine::Thought { text } => rsx! {
                        span { key: "{index}", class: "plan-think-box__line",
                            span { class: "plan-think-box__result", "> " }
                            "{text}"
                        }
                    },
                }
            }
            if phase == StepPhase::Running {
                span { class: "plan-think-box__line plan-think-box__cursor" }
            }
        }
    }
}
