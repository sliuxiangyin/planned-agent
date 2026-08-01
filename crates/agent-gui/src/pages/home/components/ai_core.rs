//! AI Core 中央脉冲核心组件。
//!
//! 指挥中心的视觉焦点：多层同心圆 + 脉冲波纹 + 粒子轨道。

use dioxus::prelude::*;

/// AI Core 组件
///
/// - `is_active`: AI 系统是否就绪（影响动画颜色）
/// - `active_plan_count`: 活跃计划数（影响内部文字）
#[component]
pub fn AiCore(
    is_active: bool,
    active_plan_count: usize,
) -> Element {
    let core_class = if is_active {
        "cc-core cc-core--active"
    } else {
        "cc-core cc-core--loading"
    };

    let count_display = if is_active {
        active_plan_count.to_string()
    } else {
        "—".to_string()
    };

    rsx! {
        div { class: "cc-core-container",
            // 外层轨道环（静态装饰）
            svg { class: "cc-core__orbit-ring",
                view_box: "0 0 300 300",
                width: "300",
                height: "300",
                circle {
                    cx: "150",
                    cy: "150",
                    r: "130",
                    fill: "none",
                    stroke: "var(--secondary-color-6)",
                    stroke_width: "0.5",
                    stroke_dasharray: "4 8",
                    opacity: "0.3",
                }
                circle {
                    cx: "150",
                    cy: "150",
                    r: "110",
                    fill: "none",
                    stroke: "var(--secondary-color-6)",
                    stroke_width: "0.5",
                    stroke_dasharray: "2 12",
                    opacity: "0.2",
                }
                // 外轨上的光点
                circle {
                    cx: "280",
                    cy: "150",
                    r: "3",
                    fill: "var(--secondary-color-3)",
                    class: "cc-core__orbit-dot",
                    opacity: "0.6",
                }
                circle {
                    cx: "20",
                    cy: "150",
                    r: "2",
                    fill: "var(--secondary-color-5)",
                    class: "cc-core__orbit-dot--slow",
                    opacity: "0.4",
                }
            }

            // 脉冲波纹（多层）
            div { class: "cc-core__ripple cc-core__ripple--1" }
            div { class: "cc-core__ripple cc-core__ripple--2" }
            div { class: "cc-core__ripple cc-core__ripple--3" }

            // 核心球体
            div { class: "{core_class}",
                // 内部发光层
                div { class: "cc-core__glow" }

                // 旋转粒子轨道
                div { class: "cc-core__particle-ring",
                    div { class: "cc-core__particle cc-core__particle--1" }
                    div { class: "cc-core__particle cc-core__particle--2" }
                    div { class: "cc-core__particle cc-core__particle--3" }
                }

                // 中心文字
                div { class: "cc-core__inner",
                    span { class: "cc-core__status-icon",
                        if is_active { "◈" } else { "◇" }
                    }
                    span { class: "cc-core__label",
                        if is_active { "AI" } else { "..." }
                    }
                    span { class: "cc-core__count",
                        "{count_display}"
                    }
                    span { class: "cc-core__sublabel", "活跃计划" }
                }
            }
        }
    }
}
