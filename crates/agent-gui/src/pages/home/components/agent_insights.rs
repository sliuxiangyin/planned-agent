//! Agent 洞察面板：右侧展示 AI 的主动建议与统计。

use dioxus::prelude::*;

use super::super::types::AgentInsight;

#[component]
pub fn AgentInsightsPanel(
    insights: Vec<AgentInsight>,
    on_action: EventHandler<String>,
) -> Element {
    rsx! {
        aside { class: "cc-insights-panel",
            // 面板标题
            div { class: "cc-insights__header",
                span { class: "cc-insights__icon", "📡" }
                h3 { class: "cc-insights__title", "Agent 洞察" }
                // 脉冲小点（表示 Agent 在监控）
                div { class: "cc-insights__live-dot" }
                span { class: "cc-insights__live-text", "实时" }
            }

            // 洞察列表
            div { class: "cc-insights__list",
                for insight in &insights {
                    {
                        let urgency_class = insight.urgency.css_class();
                        let has_action = insight.action_label.is_some();
                        let label = insight.action_label.clone();
                        let id = insight.id.clone();

                        rsx! {
                            div {
                                class: "cc-insight-card cc-insight-card--{urgency_class}",
                                // 左侧色条
                                div { class: "cc-insight-card__bar" }
                                div { class: "cc-insight-card__body",
                                    p { class: "cc-insight-card__message",
                                        "{insight.message}"
                                    }
                                    if has_action {
                                        button {
                                            class: "cc-insight-card__action",
                                            onclick: move |_| on_action.call(id.clone()),
                                            "{label.as_ref().unwrap()}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 预留入口
            div { class: "cc-insights__reserved",
                div { class: "cc-insights__reserved-item",
                    span { class: "cc-insights__reserved-icon", "⏰" }
                    span { "定时执行" }
                    span { class: "cc-insights__badge", "即将上线" }
                }
                div { class: "cc-insights__reserved-item",
                    span { class: "cc-insights__reserved-icon", "🔗" }
                    span { "多策略执行" }
                    span { class: "cc-insights__badge", "即将上线" }
                }
                div { class: "cc-insights__reserved-item",
                    span { class: "cc-insights__reserved-icon", "⚙" }
                    span { "设置" }
                    span { class: "cc-insights__badge", "即将上线" }
                }
            }
        }
    }
}
