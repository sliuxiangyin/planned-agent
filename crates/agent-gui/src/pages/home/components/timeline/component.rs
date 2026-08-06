//! 时间线组件：底部展示今日计划的时间轴。

use dioxus::prelude::*;

use crate::pages::home::types::TimelineEntry;

#[css_module("/src/pages/home/components/timeline/style.css")]
struct Styles;

#[component]
pub fn TimelineBar(
    entries: Vec<TimelineEntry>,
    on_click_entry: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: Styles::cc_timeline,
            // 时间线标签
            div { class: Styles::cc_timeline__label,
                span { class: Styles::cc_timeline__label_icon, "◷" }
                span { class: Styles::cc_timeline__label_text, "今日时间线" }
            }

            // 时间线轨道
            div { class: Styles::cc_timeline__track,
                // 时间线线条
                div { class: Styles::cc_timeline__line }

                // 当前时间指示器
                div { class: Styles::cc_timeline__now,
                    div { class: Styles::cc_timeline__now_dot }
                    div { class: Styles::cc_timeline__now_label, "现在" }
                }

                // 条目节点
                for entry in &entries {
                    {
                        let has_plan = entry.plan_id.is_some();
                        let pid = entry.plan_id.clone();
                        // 节点样式类：基础类用 Styles::，状态修饰符用 format! 拼接（__CssIdent 与 &str 类型不兼容）
                        let node_class = format!(
                            "{}{}{}",
                            Styles::cc_timeline__node,
                            if entry.is_active { format!(" {}", Styles::cc_timeline__node__past) } else { format!(" {}", Styles::cc_timeline__node__future) },
                            if has_plan { format!(" {}", Styles::cc_timeline__node__clickable) } else { String::new() },
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
                                div { class: Styles::cc_timeline__node_dot }
                                // 时间
                                span { class: Styles::cc_timeline__node_time, "{entry.time}" }
                                // 计划名
                                span { class: Styles::cc_timeline__node_name, "{entry.plan_name}" }
                                // 底部小竖线
                                div { class: Styles::cc_timeline__node_tick }
                            }
                        }
                    }
                }
            }
        }
    }
}