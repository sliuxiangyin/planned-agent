//! PIPELINE Bento 块：执行时间线。
//!
//! 展示的是**已按当前参数值展开**的步骤文本（展开在 `PlanLeftPanel::render_steps`），
//! 所以 PARAMS 里一改参数，这里立刻跟着变。未填的参数保留 `${name}` 原样并给出提示。
//!
//! 三态与 THINK 终端由执行器的 `PlanRunEvent` 驱动，属于执行接线；
//! 在此之前所有步骤渲染为 pending。

use dioxus::prelude::*;

use super::left_panel::{missing_label, RenderedStep};

#[component]
pub fn PipelineView(steps: Vec<RenderedStep>, hint: Option<String>) -> Element {
    let total = steps.len();
    let empty_hint = hint.or_else(|| steps.is_empty().then(|| "该模板未定义步骤".to_string()));

    rsx! {
        div { class: "plan-bento-block",
            div { class: "plan-bento-block__header",
                span { class: "plan-bento-block__header-emoji", "🎯" }
                span { class: "plan-bento-block__header-label", "PIPELINE" }
                span { class: "plan-bento-block__header-spacer" }
                // 历史按钮
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
                // 执行按钮
                button {
                    class: "plan-bento-header-btn",
                    title: "执行计划",
                    svg {
                        xmlns: "http://www.w3.org/2000/svg",
                        width: "14",
                        height: "14",
                        view_box: "0 0 16 16",
                        fill: "currentColor",
                        path { d: "M 4 2.5 L 13 8 L 4 13.5 Z" }
                    }
                }
                // 停止按钮
                button {
                    class: "plan-bento-header-btn",
                    title: "停止执行",
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
                                class: "plan-pipeline__step plan-pipeline__step--pending",
                                key: "{index}",
                                div { class: "plan-pipeline__step-rail",
                                    div { class: "plan-pipeline__step-dot",
                                        svg {
                                            class: "plan-pipeline-node--pending",
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
                                    div { class: "plan-pipeline__step-line plan-pipeline__step-line--pending" }
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
                                }
                            }
                        }
                    }

                    // 底部状态栏：执行前只有步骤总数，其余指标待执行后回填。
                    div { class: "plan-pipeline__statusbar",
                        span { class: "plan-pipeline__statusbar-item", "0/{total} steps" }
                        span { class: "plan-pipeline__statusbar-item",
                            "⏱ "
                            span { class: "plan-pipeline__statusbar-val", "—" }
                        }
                        span { class: "plan-pipeline__statusbar-item",
                            "🔧 "
                            span { class: "plan-pipeline__statusbar-val", "—" }
                            " calls"
                        }
                        span { class: "plan-pipeline__statusbar-item",
                            "📝 "
                            span { class: "plan-pipeline__statusbar-val", "—" }
                            " tk"
                        }
                    }
                }
            }
        }
    }
}
