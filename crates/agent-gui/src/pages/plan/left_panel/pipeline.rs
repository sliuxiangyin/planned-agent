//! PIPELINE Bento 块：执行时间线。
//!
//! 展示 4 步流水线（获取结果 → 分析依赖 → 生成报告 → 验证输出）的
//! 当前状态（done / running + THINK 终端 / pending）与底部状态栏。
//! 当前为静态 mock，后续接入 `WorkflowState` 驱动真实进度。

use dioxus::prelude::*;

#[component]
pub fn PipelineView() -> Element {
    rsx! {
        div { class: "plan-bento-block",
            div { class: "plan-bento-block__header",
                span { class: "plan-bento-block__header-emoji", "🎯" }
                span { class: "plan-bento-block__header-label", "PIPELINE" }
                span { class: "plan-bento-chip plan-bento-chip--version", "v3" }
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
                div { class: "plan-pipeline__timeline",
                    // Step 1 — done
                    div { class: "plan-pipeline__step plan-pipeline__step--done",
                        div { class: "plan-pipeline__step-rail",
                            div { class: "plan-pipeline__step-dot",
                                svg {
                                    class: "plan-pipeline-node--done",
                                    xmlns: "http://www.w3.org/2000/svg",
                                    view_box: "0 0 16 16",
                                    width: "14",
                                    height: "14",
                                    fill: "none",
                                    stroke: "currentColor",
                                    stroke_width: "1.75",
                                    stroke_linecap: "round",
                                    stroke_linejoin: "round",
                                    circle { cx: "8", cy: "8", r: "6" }
                                    path { d: "M 5 8.5 L 7 10.5 L 11 6" }
                                }
                            }
                            div { class: "plan-pipeline__step-line plan-pipeline__step-line--done" }
                        }
                        div { class: "plan-pipeline__step-body",
                            div { class: "plan-pipeline__step-header",
                                span { class: "plan-pipeline__step-index", "S1" }
                                span { class: "plan-pipeline__step-title", "获取搜索结果" }
                                span { class: "plan-pipeline__step-meta", "2.3s · 3 tools" }
                            }
                            div { class: "plan-pipeline__step-detail", "预期输出: 结构化的搜索结果 JSON 数组" }
                        }
                    }

                    // Step 2 — running + think box
                    div { class: "plan-pipeline__step plan-pipeline__step--running",
                        div { class: "plan-pipeline__step-rail",
                            div { class: "plan-pipeline__step-dot",
                                svg {
                                    class: "plan-pipeline-node--running",
                                    xmlns: "http://www.w3.org/2000/svg",
                                    view_box: "0 0 16 16",
                                    width: "14",
                                    height: "14",
                                    fill: "none",
                                    stroke: "currentColor",
                                    stroke_width: "1.75",
                                    stroke_linecap: "round",
                                    path { d: "M 8 2 A 6 6 0 1 1 2 8" }
                                }
                            }
                            div { class: "plan-pipeline__step-line plan-pipeline__step-line--active" }
                        }
                        div { class: "plan-pipeline__step-body",
                            div { class: "plan-pipeline__step-header",
                                span { class: "plan-pipeline__step-index", "S2" }
                                span { class: "plan-pipeline__step-title", "分析数据依赖" }
                                span { class: "plan-pipeline__step-meta", "⬤ RUNNING · 4.1s" }
                            }
                            // THINK 终端
                            div { class: "plan-think-box",
                                span { class: "plan-think-box__line",
                                    span { class: "plan-think-box__prompt", "$ " }
                                    "analyze_deps --scope step_1_output"
                                }
                                span { class: "plan-think-box__line",
                                    span { class: "plan-think-box__result", "> " }
                                    "解析 Step 1 返回的 3 个端点..."
                                }
                                span { class: "plan-think-box__line",
                                    span { class: "plan-think-box__result", "> " }
                                    "检测到 /internal/docs 缺少 auth header"
                                }
                                span { class: "plan-think-box__line",
                                    span { class: "plan-think-box__result", "> " }
                                    "自动调用 search_docs(\"auth\") 补充..."
                                }
                                span { class: "plan-think-box__line",
                                    span { class: "plan-think-box__result", "> " }
                                    "收到 127 行文档，提取 Bearer token 格式"
                                }
                                span { class: "plan-think-box__line",
                                    span { class: "plan-think-box__result", "> " }
                                    "决策: 并入 Step 3 输出前验证"
                                }
                                span { class: "plan-think-box__line plan-think-box__cursor" }
                            }
                        }
                    }

                    // Step 3 — pending
                    div { class: "plan-pipeline__step plan-pipeline__step--pending",
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
                                span { class: "plan-pipeline__step-index", "S3" }
                                span { class: "plan-pipeline__step-title", "生成最终报告" }
                            }
                            div { class: "plan-pipeline__step-detail", "预期输出: Markdown 格式的综合分析报告" }
                        }
                    }

                    // Step 4 — pending
                    div { class: "plan-pipeline__step plan-pipeline__step--pending",
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
                                span { class: "plan-pipeline__step-index", "S4" }
                                span { class: "plan-pipeline__step-title", "验证并输出" }
                            }
                            div { class: "plan-pipeline__step-detail", "预期输出: 验证通过的最终结果 + 摘要" }
                        }
                    }
                }

                // 底部状态栏
                div { class: "plan-pipeline__statusbar",
                    span { class: "plan-pipeline__statusbar-item",
                        span { class: "plan-pipeline__statusbar-dot plan-pipeline__statusbar-dot--ok" }
                        "2/4 steps"
                    }
                    span { class: "plan-pipeline__statusbar-item",
                        "⏱ "
                        span { class: "plan-pipeline__statusbar-val", "6.4s" }
                    }
                    span { class: "plan-pipeline__statusbar-item",
                        "🔧 "
                        span { class: "plan-pipeline__statusbar-val", "5" }
                        " calls"
                    }
                    span { class: "plan-pipeline__statusbar-item",
                        "📝 "
                        span { class: "plan-pipeline__statusbar-val", "1.2k" }
                        " / "
                        span { class: "plan-pipeline__statusbar-val", "8.0k" }
                        " tk"
                    }
                }
            }
        }
    }
}
