//! ToolView 组件：渲染单个 Tool 调用的可折叠详情卡片。
//!
//! - 默认折叠；点击头部展开/收起
//! - 显示 tool 名称、执行状态图标（lucide SVG）
//! - 展开后显示 Input（参数）和 Output（结果/错误）
//! - 不同 phase 对应不同视觉样式（Pending 脉冲动画、Error 红色边框等）

use dioxus::prelude::*;
use dioxus_icons::lucide::{CircleCheckBig, CircleDot, CircleX, LoaderCircle};

use crate::components::chat::chat_flow::{ToolCallEntry, ToolCallPhase};

#[css_module("/src/components/chat/tool_view/style.css")]
struct Styles;

/// 单个 Tool 调用的可折叠详情卡片。
#[component]
pub fn ToolView(entry: ToolCallEntry) -> Element {
    let mut open = use_signal(|| false);
    let is_open = *open.read();

    let phase = &entry.phase;

    // 容器 class：基础 + phase 修饰符（独立类名）
    let container_class = match phase {
        ToolCallPhase::Error => format!("{} {}", Styles::tool_view, Styles::tool_view_error),
        ToolCallPhase::Pending => format!("{} {}", Styles::tool_view, Styles::tool_view_pending),
        ToolCallPhase::Running => format!("{} {}", Styles::tool_view, Styles::tool_view_running),
        _ => Styles::tool_view.to_string(),
    };

    // 格式化参数（已经是 pretty-printed JSON）
    let arguments_display = if entry.arguments.is_empty() {
        None
    } else {
        Some(entry.arguments.clone())
    };

    // 格式化结果
    let result_display = entry.result.as_ref().map(|v| {
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    });

    rsx! {
        div {
            class: "{container_class}",
            "data-open": if is_open { "true" } else { "false" },

            // ── Header ──
            div {
                class: Styles::tool_view_header,
                onclick: move |_| open.toggle(),

                // 折叠箭头
                svg {
                    class: Styles::tool_view_chevron,
                    view_box: "0 0 16 16",
                    width: "12",
                    height: "12",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "1.75",
                    stroke_linecap: "round",
                    stroke_linejoin: "round",
                    path { d: "M 6 4 L 10 8 L 6 12" }
                }

                // 状态图标（lucide SVG）
                match phase {
                    ToolCallPhase::Pending => rsx! {
                        span { class: Styles::tool_view_icon,
                            CircleDot { size: "14", class: Styles::tool_view_icon_pending }
                        }
                    },
                    ToolCallPhase::Running => rsx! {
                        span { class: Styles::tool_view_icon,
                            LoaderCircle { size: "14", class: Styles::tool_view_icon_running }
                        }
                    },
                    ToolCallPhase::Completed => rsx! {
                        span { class: Styles::tool_view_icon,
                            CircleCheckBig { size: "14", class: Styles::tool_view_icon_completed }
                        }
                    },
                    ToolCallPhase::Error => rsx! {
                        span { class: Styles::tool_view_icon,
                            CircleX { size: "14", class: Styles::tool_view_icon_error }
                        }
                    },
                }

                // Tool 名称
                span { class: Styles::tool_view_name, "{entry.name}" }
            }

            // ── 展开内容 ──
            if is_open {
                div {
                    class: Styles::tool_view_content,

                    // Input 区域
                    if let Some(ref args) = arguments_display {
                        div {
                            class: Styles::tool_view_section,
                            div { class: Styles::tool_view_section_label, "Input" }
                            pre { class: Styles::tool_view_code, "{args}" }
                        }
                    }

                    // Output 区域
                    if let Some(ref result) = result_display {
                        div {
                            class: Styles::tool_view_section,
                            div { class: Styles::tool_view_section_label, "Output" }
                            pre { class: Styles::tool_view_code, "{result}" }
                        }
                    } else if phase == &ToolCallPhase::Pending || phase == &ToolCallPhase::Running {
                        div {
                            class: Styles::tool_view_section,
                            div { class: Styles::tool_view_section_label, "Output" }
                            div { class: Styles::tool_view_code, "等待执行…" }
                        }
                    }
                }
            }
        }
    }
}
