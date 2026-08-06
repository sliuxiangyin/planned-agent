//! AI Core 中央脉冲核心组件。
//!
//! 指挥中心的视觉焦点：多层同心圆 + 脉冲波纹 + 粒子轨道。

use dioxus::prelude::*;

#[css_module("/src/pages/home/components/ai_core/style.css")]
struct Styles;

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
        format!("{} {}", Styles::cc_core, Styles::cc_core__active)
    } else {
        format!("{} {}", Styles::cc_core, Styles::cc_core__loading)
    };

    let count_display = if is_active {
        active_plan_count.to_string()
    } else {
        "—".to_string()
    };

    rsx! {
        div { class: Styles::cc_core_container,
            // 外层轨道环（静态装饰）
            svg { class: Styles::cc_core__orbit_ring,
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
                    class: Styles::cc_core__orbit_dot,
                    opacity: "0.6",
                }
                circle {
                    cx: "20",
                    cy: "150",
                    r: "2",
                    fill: "var(--secondary-color-5)",
                    class: Styles::cc_core__orbit_dot__slow,
                    opacity: "0.4",
                }
            }

            // 脉冲波纹（多层）
            div { class: "{Styles::cc_core__ripple} {Styles::cc_core__ripple__1}" }
            div { class: "{Styles::cc_core__ripple} {Styles::cc_core__ripple__2}" }
            div { class: "{Styles::cc_core__ripple} {Styles::cc_core__ripple__3}" }

            // 核心球体
            div { class: "{core_class}",
                // 内部发光层
                div { class: Styles::cc_core__glow }

                // 旋转粒子轨道
                div { class: Styles::cc_core__particle_ring,
                    div { class: "{Styles::cc_core__particle} {Styles::cc_core__particle__1}" }
                    div { class: "{Styles::cc_core__particle} {Styles::cc_core__particle__2}" }
                    div { class: "{Styles::cc_core__particle} {Styles::cc_core__particle__3}" }
                }

                // 中心文字
                div { class: Styles::cc_core__inner,
                    span { class: Styles::cc_core__status_icon,
                        if is_active { "◈" } else { "◇" }
                    }
                    span { class: Styles::cc_core__label,
                        if is_active { "AI" } else { "..." }
                    }
                    span { class: Styles::cc_core__count,
                        "{count_display}"
                    }
                    span { class: Styles::cc_core__sublabel, "活跃计划" }
                }
            }
        }
    }
}