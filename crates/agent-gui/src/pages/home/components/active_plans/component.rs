//! Active Plans — 环绕 AI Core 的计划节点。
//!
//! 每个计划在轨道环上以角度定位，hover 时高亮+放大，click 进入编辑。

use dioxus::prelude::*;

use crate::pages::home::types::{PlanMeta, PlanStatus};

#[css_module("/src/pages/home/components/active_plans/style.css")]
struct Styles;

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
        div { class: Styles::cc_active_plans,
            for plan in &plans {
                {
                    let pos = orbit_position(plan.orbit_angle, plan.status.orbit_level());
                    let is_hovered = hovered_id.as_deref() == Some(&plan.id);
                    let is_completed = plan.status == PlanStatus::Generated;

                    // 节点样式类：基础类用 Styles::，状态/交互修饰符用 format! 拼接（__CssIdent 与 &str 类型不兼容）
                    let node_class = format!(
                        "{} cc-plan-node--{}{}{}",
                        Styles::cc_plan_node,
                        plan.status.css_class(),
                        if is_hovered { format!(" {}", Styles::cc_plan_node__hovered) } else { String::new() },
                        if is_completed { format!(" {}", Styles::cc_plan_node__faded) } else { String::new() },
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
                            div { class: Styles::cc_plan_node__line }

                            // 节点内容
                            div { class: Styles::cc_plan_node__content,
                                // 状态指示点
                                div { class: Styles::cc_plan_node__dot }

                                // 计划名
                                span { class: Styles::cc_plan_node__name, "{plan.name}" }

                                // hover 展开详情
                                if is_hovered {
                                    div { class: Styles::cc_plan_node__detail,
                                        span { class: Styles::cc_plan_node__status_label,
                                            "{plan.status.label()}"
                                        }
                                        if let Some(ref sched) = plan.schedule {
                                            span { class: "{Styles::cc_plan_node__tag} {Styles::cc_plan_node__tag__schedule}",
                                                "⏰ {sched.description}"
                                            }
                                        }
                                        if let Some(ref strategy) = plan.strategy {
                                            span { class: "{Styles::cc_plan_node__tag} {Styles::cc_plan_node__tag__strategy}",
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