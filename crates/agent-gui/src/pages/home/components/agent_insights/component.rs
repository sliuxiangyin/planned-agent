//! Agent 洞察面板：右侧展示 AI 的主动建议与统计。

use dioxus::prelude::*;

use crate::pages::home::types::AgentInsight;

#[css_module("/src/pages/home/components/agent_insights/style.css")]
struct Styles;

#[component]
pub fn AgentInsightsPanel(
    insights: Vec<AgentInsight>,
    on_action: EventHandler<String>,
) -> Element {
    rsx! {
        aside { class: Styles::cc_insights_panel,
            // 面板标题
            div { class: Styles::cc_insights__header,
                span { class: Styles::cc_insights__icon, "📡" }
                h3 { class: Styles::cc_insights__title, "Agent 洞察" }
                // 脉冲小点（表示 Agent 在监控）
                div { class: Styles::cc_insights__live_dot }
                span { class: Styles::cc_insights__live_text, "实时" }
            }

            // 洞察列表
            div { class: Styles::cc_insights__list,
                for insight in &insights {
                    {
                        let urgency_class = insight.urgency.css_class();
                        let has_action = insight.action_label.is_some();
                        let label = insight.action_label.clone();
                        let id = insight.id.clone();

                        rsx! {
                            div {
                                // 基础类用 Styles::，紧急程度修饰符仍用字面量拼接（css_module 只对 Styles::* 生效）
                                class: "{Styles::cc_insight_card} cc-insight-card--{urgency_class}",
                                // 左侧色条
                                div { class: Styles::cc_insight_card__bar }
                                div { class: Styles::cc_insight_card__body,
                                    p { class: Styles::cc_insight_card__message,
                                        "{insight.message}"
                                    }
                                    if has_action {
                                        button {
                                            class: Styles::cc_insight_card__action,
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
            div { class: Styles::cc_insights__reserved,
                div { class: Styles::cc_insights__reserved_item,
                    span { class: Styles::cc_insights__reserved_icon, "⏰" }
                    span { "定时执行" }
                    span { class: Styles::cc_insights__badge, "即将上线" }
                }
                div { class: Styles::cc_insights__reserved_item,
                    span { class: Styles::cc_insights__reserved_icon, "🔗" }
                    span { "多策略执行" }
                    span { class: Styles::cc_insights__badge, "即将上线" }
                }
                div { class: Styles::cc_insights__reserved_item,
                    span { class: Styles::cc_insights__reserved_icon, "⚙" }
                    span { "设置" }
                    span { class: Styles::cc_insights__badge, "即将上线" }
                }
            }
        }
    }
}