//! AgentView 组件：子 agent 输出的嵌入式卡片。
//!
//! - 名称 + 状态图标始终展示在左上角
//! - 内容始终展开，不支持折叠
//! - 顶部渐变线 + Bot 图标标识「这是子 agent 输出」
//! - 以 Markdown 渲染子 agent 的流式文本
//! - Pending（脉冲）→ Running（旋转）→ Completed（对勾）→ Error（红色）

use dioxus::prelude::*;
use dioxus_icons::lucide::{Bot, CircleCheckBig, CircleDot, CircleX, LoaderCircle};

use crate::components::chat::chat_flow::{AgentEvent, AgentViewData, ToolCallPhase};

#[css_module("/src/components/chat/agent_view/style.css")]
struct Styles;

/// 子 agent 输出的嵌入式卡片。
#[component]
pub fn AgentView(data: AgentViewData) -> Element {
    let phase = &data.phase;

    // 容器 class：基础 + phase 修饰符
    let container_class = match phase {
        ToolCallPhase::Pending => format!("{} {}", Styles::agent_view, Styles::agent_view__pending),
        ToolCallPhase::Running => format!("{} {}", Styles::agent_view, Styles::agent_view__running),
        ToolCallPhase::Error => format!("{} {}", Styles::agent_view, Styles::agent_view__error),
        _ => Styles::agent_view.to_string(),
    };

    // 拼接 events 为文本（用于 Markdown 渲染）
    let body_text: String = data
        .events
        .iter()
        .map(|ev| match ev {
            AgentEvent::TextDelta(s) => s.as_str(),
            AgentEvent::ReasoningDelta(s) => s.as_str(),
        })
        .collect();

    rsx! {
        div {
            class: "{container_class}",

            // ── 顶部渐变线 ──
            div { class: Styles::agent_view__accent_line }

            // ── Header：名称（左）+ 状态图标（右） ──
            div {
                class: Styles::agent_view__header,

                // Bot 图标
                span { class: Styles::agent_view__bot_icon,
                    Bot { size: "14" }
                }

                // Agent 名称
                span { class: Styles::agent_view__name, "{data.name}" }

                // 状态图标（右对齐）
                match phase {
                    ToolCallPhase::Pending => rsx! {
                        span { class: Styles::agent_view__status_icon,
                            CircleDot { size: "14", class: Styles::agent_view__status_pending }
                        }
                    },
                    ToolCallPhase::Running => rsx! {
                        span { class: Styles::agent_view__status_icon,
                            LoaderCircle { size: "14", class: Styles::agent_view__status_running }
                        }
                    },
                    ToolCallPhase::Completed => rsx! {
                        span { class: Styles::agent_view__status_icon,
                            CircleCheckBig { size: "14", class: Styles::agent_view__status_completed }
                        }
                    },
                    ToolCallPhase::Error => rsx! {
                        span { class: Styles::agent_view__status_icon,
                            CircleX { size: "14", class: Styles::agent_view__status_error }
                        }
                    },
                }
            }

            // ── 内容（始终展示） ──
            div { class: Styles::agent_view__body,
                if body_text.is_empty() {
                    if data.is_streaming {
                        span { class: Styles::agent_view__streaming, "" }
                    } else {
                        div { class: Styles::agent_view__empty, "（无输出）" }
                    }
                } else {
                    crate::components::markdown::Markdown { text: body_text.clone() }
                    if data.is_streaming {
                        span { class: Styles::agent_view__streaming, "" }
                    }
                }
            }
        }
    }
}
