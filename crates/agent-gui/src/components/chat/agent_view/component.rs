//! AgentView 组件：子 agent 输出的嵌入式卡片。
//!
//! - 名称 + 状态图标始终展示在左上角
//! - 内容始终展开，不支持折叠
//! - 顶部渐变线 + Bot 图标标识「这是子 agent 输出」
//! - 以 Markdown 渲染子 agent 的流式文本
//! - 子 agent 内部工具调用按消息流顺序穿插在文本之间（轻量：工具名 + 状态图标）
//! - Pending（脉冲）→ Running（旋转）→ Completed（对勾）→ Error（红色）

use dioxus::prelude::*;
use dioxus_icons::lucide::{Bot, CircleCheckBig, CircleDot, CircleX, LoaderCircle};

use crate::components::chat::chat_flow::{AgentEvent, AgentViewData, ToolCallPhase};

#[css_module("/src/components/chat/agent_view/style.css")]
struct Styles;

/// 渲染分段：连续文本合并为一段，工具调用单独成段（保持消息流顺序）。
enum Segment {
    Text(String),
    Tool { name: String, phase: ToolCallPhase },
}

/// 把 `events` 按顺序拆成渲染分段：连续文本合并为一段，工具调用单独成段，
/// 从而让工具调用按消息流顺序穿插在文本之间，而不是堆积在底部。
fn build_segments(events: &[AgentEvent]) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();
    for ev in events {
        match ev {
            AgentEvent::TextDelta(s) | AgentEvent::ReasoningDelta(s) => match segs.last_mut() {
                Some(Segment::Text(t)) => t.push_str(s),
                _ => segs.push(Segment::Text(s.clone())),
            },
            AgentEvent::ToolCall { name, phase, .. } => {
                segs.push(Segment::Tool {
                    name: name.clone(),
                    phase: phase.clone(),
                });
            }
        }
    }
    segs
}

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

    // 按消息流顺序拆分渲染分段（文本 + 工具调用穿插）
    let segments = build_segments(&data.events);

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

            // ── 内容（始终展示；文本与工具调用按消息流顺序穿插） ──
            // data-agent-streaming 标记：让 ChatPanel 的滚动 effect 能在流式期间
            // 把本卡片内部滚动容器自动滚到底（随最新输出跟随）。
            div { class: Styles::agent_view__body, "data-agent-streaming": "{data.is_streaming}",
                if segments.is_empty() {
                    if data.is_streaming {
                        span { class: Styles::agent_view__streaming, "" }
                    } else {
                        div { class: Styles::agent_view__empty, "（无输出）" }
                    }
                } else {
                    for seg in &segments {
                        match seg {
                            Segment::Text(t) => rsx! {
                                crate::components::markdown::Markdown { text: t.clone() }
                            },
                            Segment::Tool { name, phase } => rsx! {
                                div { class: Styles::agent_view__tool,
                                    span { class: Styles::agent_view__tool_icon,
                                        match phase {
                                            ToolCallPhase::Pending => rsx! {
                                                CircleDot { size: "12", class: Styles::agent_view__status_pending }
                                            },
                                            ToolCallPhase::Running => rsx! {
                                                LoaderCircle { size: "12", class: Styles::agent_view__status_running }
                                            },
                                            ToolCallPhase::Completed => rsx! {
                                                CircleCheckBig { size: "12", class: Styles::agent_view__status_completed }
                                            },
                                            ToolCallPhase::Error => rsx! {
                                                CircleX { size: "12", class: Styles::agent_view__status_error }
                                            },
                                        }
                                    }
                                    span { class: Styles::agent_view__tool_name, "{name}" }
                                }
                            },
                        }
                    }
                    if data.is_streaming {
                        span { class: Styles::agent_view__streaming, "" }
                    }
                }
            }
        }
    }
}
