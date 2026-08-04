//! 指挥中心主组件。
//!
//! 布局：顶栏 → 核心区(左:AI Core+计划节点 | 右:Agent洞察) → 操作栏 → 时间线

use dioxus::prelude::*;

use crate::context::{InitStatus, ModuleState};

use super::components::agent_insights::AgentInsightsPanel;
use super::components::ai_core::AiCore;
use super::components::active_plans::ActivePlans;
use super::components::quick_actions::QuickActionsBar;
use super::components::timeline::TimelineBar;
use super::types::{mock_insights, mock_plans, mock_timeline, PlanMeta};

/// 本页面专属样式（按需加载，不再由 main.rs 统一引入）。
const HOME_CSS: Asset = asset!("/assets/home.css");

#[derive(Clone, PartialEq)]
pub enum PageRoute {
    Home,
    Plan(Option<String>), // None = 新建, Some(id) = 编辑已有
    Settings,
    // MCP 服务改用嵌套布局：左侧 nav 不动，右侧切视图（在 SettingsPage 内部用 McpView state 管理），
    // 不再是顶级 page 路由。
}

#[component]
pub fn HomePage(
    on_navigate: EventHandler<PageRoute>,
) -> Element {
    let init_status = use_context::<Memo<InitStatus>>();
    let plans = use_signal(mock_plans);
    let insights = use_signal(mock_insights);
    let timeline = use_signal(mock_timeline);

    // hover 的计划 id（用于高亮轨道节点）
    let mut hovered_plan = use_signal(|| None::<String>);

    // 状态指示器
    let ai_ready = init_status.read().ai.state == ModuleState::Ready;
    let mcp_ready = init_status.read().mcp.state == ModuleState::Ready;
    let prompt_ready = init_status.read().prompt.state == ModuleState::Ready;

    let all_ready = ai_ready && mcp_ready && prompt_ready;

    // 活跃计划数统计
    let active_count = plans.read().iter().filter(|p| matches!(p.status, super::types::PlanStatus::Running | super::types::PlanStatus::Queued)).count();
    let completed_count = plans.read().iter().filter(|p| matches!(p.status, super::types::PlanStatus::Completed)).count();

    rsx! {
        document::Stylesheet { href: HOME_CSS }
        div { class: "command-center",
            // ═══════════════════════════════════════════════════════
            // 顶栏
            // ═══════════════════════════════════════════════════════
            header { class: "cc-topbar",
                div { class: "cc-topbar__left",
                    span { class: "cc-topbar__logo", "◆" }
                    h1 { class: "cc-topbar__title", "Planned Agent" }
                    span { class: "cc-topbar__subtitle", "AI 指挥中心" }
                }
                div { class: "cc-topbar__center",
                    // 全局状态脉冲指示
                    div {
                        class: format!(
                            "cc-status-pulse {}",
                            if all_ready { "cc-status-pulse--ready" } else { "cc-status-pulse--loading" }
                        ),
                    }
                    span { class: "cc-status-text",
                        if all_ready { "所有系统就绪" } else { "系统初始化中..." }
                    }
                }
                div { class: "cc-topbar__right",
                    div {
                        class: format!("cc-module-dot {}", if ai_ready { "ready" } else { "init" }),
                        title: if ai_ready { "AI 已连接" } else { "AI 初始化中" },
                    }
                    div {
                        class: format!("cc-module-dot {}", if mcp_ready { "ready" } else { "init" }),
                        title: if mcp_ready { "MCP 已连接" } else { "MCP 初始化中" },
                    }
                    div {
                        class: format!("cc-module-dot {}", if prompt_ready { "ready" } else { "init" }),
                        title: if prompt_ready { "Prompt 已加载" } else { "Prompt 加载中" },
                    }
                    // 设置入口
                    button {
                        class: "cc-settings-btn",
                        title: "设置",
                        onclick: move |_| on_navigate.call(PageRoute::Settings),
                        "⚙"
                    }
                }
            }

            // ═══════════════════════════════════════════════════════
            // 核心区
            // ═══════════════════════════════════════════════════════
            main { class: "cc-main",
                // ── 左侧：AI Core + 环绕计划 ──
                div { class: "cc-core-area",
                    // 统计概览（左上角）
                    div { class: "cc-stats-overlay",
                        div { class: "cc-stat",
                            span { class: "cc-stat__value", "{active_count}" }
                            span { class: "cc-stat__label", "活跃" }
                        }
                        div { class: "cc-stat",
                            span { class: "cc-stat__value", "{completed_count}" }
                            span { class: "cc-stat__label", "完成" }
                        }
                        div { class: "cc-stat",
                            span { class: "cc-stat__value", "{timeline.read().len()}" }
                            span { class: "cc-stat__label", "今日" }
                        }
                    }

                    // AI Core（中心脉冲）
                    AiCore {
                        is_active: all_ready,
                        active_plan_count: active_count,
                    }

                    // 环绕计划节点
                    ActivePlans {
                        plans: plans.read().clone(),
                        hovered_id: hovered_plan.read().clone(),
                        on_hover: move |id: Option<String>| hovered_plan.set(id),
                        on_click: move |plan: PlanMeta| {
                            on_navigate.call(PageRoute::Plan(Some(plan.id)));
                        },
                    }
                }

                // ── 右侧：Agent 洞察面板 ──
                AgentInsightsPanel {
                    insights: insights.read().clone(),
                    on_action: move |insight_id: String| {
                        tracing::info!("洞察操作: {}", insight_id);
                        // TODO: 后续接入实际操作
                    },
                }
            }

            // ═══════════════════════════════════════════════════════
            // 快捷操作栏
            // ═══════════════════════════════════════════════════════
            QuickActionsBar {
                on_new_plan: move |_| on_navigate.call(PageRoute::Plan(None)),
                on_execute_all: move |_| {
                    tracing::info!("一键执行全部计划");
                },
                on_voice_input: move |_| {
                    tracing::info!("语音输入（待实现）");
                },
            }

            // ═══════════════════════════════════════════════════════
            // 底部时间线
            // ═══════════════════════════════════════════════════════
            TimelineBar {
                entries: timeline.read().clone(),
                on_click_entry: move |plan_id: String| {
                    on_navigate.call(PageRoute::Plan(Some(plan_id)));
                },
            }
        }
    }
}
