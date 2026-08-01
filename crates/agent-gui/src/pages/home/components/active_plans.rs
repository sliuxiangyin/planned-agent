//! Active Plans — 环绕 AI Core 的计划节点。
//!
//! 每个计划在轨道环上以角度定位，hover 时高亮+放大，click 进入编辑。

use dioxus::prelude::*;

use super::super::types::{PlanMeta, PlanStatus};

/// 根据角度和轨道层级计算 CSS 定位的 (x%, y%)
fn orbit_position(angle_deg: f64, level: usize) -> (f64, f64) {
    let angle_rad = angle_deg.to_radians();
    // 不同层级的半径
    let radius = match level {
        0 => 38.0, // Running: 最近
        1 => 48.0, // Queued
        2 => 58.0, // Paused / Failed
        _ => 68.0, // Completed: 最远
    };
    // 中心 50%, 坐标偏移
    let x = 50.0 + radius * angle_rad.cos();
    let y = 50.0 + radius * angle_rad.sin();
    (x, y)
}

#[component]
pub fn ActivePlans(
    plans: Vec<PlanMeta>,
    hovered_id: Option<String>,
    on_hover: EventHandler<Option<String>>,
    on_click: EventHandler<PlanMeta>,
) -> Element {
    rsx! {
        div { class: "cc-active-plans",
            for plan in &plans {
                {
                    let pos = orbit_position(plan.orbit_angle, plan.status.orbit_level());
                    let is_hovered = hovered_id.as_deref() == Some(&plan.id);
                    let is_completed = plan.status == PlanStatus::Completed;

                    // 节点样式类
                    let node_class = format!(
                        "cc-plan-node cc-plan-node--{} {} {}",
                        plan.status.css_class(),
                        if is_hovered { "cc-plan-node--hovered" } else { "" },
                        if is_completed { "cc-plan-node--faded" } else { "" },
                    );

                    let plan_id = plan.id.clone();
                    let plan_clone = plan.clone();

                    rsx! {
                        div {
                            class: "{node_class}",
                            style: "left: {pos.0}%; top: {pos.1}%;",
                            onmouseenter: move |_| on_hover.call(Some(plan_id.clone())),
                            onmouseleave: move |_| on_hover.call(None),
                            onclick: move |_| on_click.call(plan_clone.clone()),

                            // 连接线（从节点到核心）
                            div { class: "cc-plan-node__line" }

                            // 节点内容
                            div { class: "cc-plan-node__content",
                                // 状态指示点
                                div { class: "cc-plan-node__dot" }

                                // 计划名
                                span { class: "cc-plan-node__name", "{plan.name}" }

                                // hover 展开详情
                                if is_hovered {
                                    div { class: "cc-plan-node__detail",
                                        span { class: "cc-plan-node__status-label",
                                            "{plan.status.label()}"
                                        }
                                        if let Some(ref sched) = plan.schedule {
                                            span { class: "cc-plan-node__tag cc-plan-node__tag--schedule",
                                                "⏰ {sched.description}"
                                            }
                                        }
                                        if let Some(ref strategy) = plan.strategy {
                                            span { class: "cc-plan-node__tag cc-plan-node__tag--strategy",
                                                "🔗 {strategy.name}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
