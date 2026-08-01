//! 时间线组件：底部展示今日计划的时间轴。

use dioxus::prelude::*;

use super::super::types::TimelineEntry;

#[component]
pub fn TimelineBar(
    entries: Vec<TimelineEntry>,
    on_click_entry: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: "cc-timeline",
            // 时间线标签
            div { class: "cc-timeline__label",
                span { class: "cc-timeline__label-icon", "◷" }
                span { class: "cc-timeline__label-text", "今日时间线" }
            }

            // 时间线轨道
            div { class: "cc-timeline__track",
                // 时间线线条
                div { class: "cc-timeline__line" }

                // 当前时间指示器
                div { class: "cc-timeline__now",
                    div { class: "cc-timeline__now-dot" }
                    div { class: "cc-timeline__now-label", "现在" }
                }

                // 条目节点
                for entry in &entries {
                    {
                        let has_plan = entry.plan_id.is_some();
                        let pid = entry.plan_id.clone();
                        let node_class = format!(
                            "cc-timeline__node {} {}",
                            if entry.is_active { "cc-timeline__node--past" } else { "cc-timeline__node--future" },
                            if has_plan { "cc-timeline__node--clickable" } else { "" },
                        );

                        rsx! {
                            div {
                                class: "{node_class}",
                                onclick: move |_| {
                                    if let Some(ref id) = pid {
                                        on_click_entry.call(id.clone());
                                    }
                                },
                                // 节点圆点
                                div { class: "cc-timeline__node-dot" }
                                // 时间
                                span { class: "cc-timeline__node-time", "{entry.time}" }
                                // 计划名
                                span { class: "cc-timeline__node-name", "{entry.plan_name}" }
                                // 底部小竖线
                                div { class: "cc-timeline__node-tick" }
                            }
                        }
                    }
                }
            }
        }
    }
}
