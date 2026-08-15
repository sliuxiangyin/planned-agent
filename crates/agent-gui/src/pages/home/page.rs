//! 指挥中心主组件。
//!
//! 布局：顶栏 → 核心区(左:AI Core+计划节点 | 右:Agent洞察) → 操作栏 → 时间线

use std::sync::Arc;

use dioxus::prelude::*;

use crate::context::{InitStatus, ModuleState, StorageContext};

use super::components::active_plans::ActivePlans;
use super::components::agent_insights::AgentInsightsPanel;
use super::components::ai_core::AiCore;
use super::components::create_plan_modal::{CreatePlanData, CreatePlanModal};
use super::components::quick_actions::QuickActionsBar;
use super::components::timeline::TimelineBar;
use super::types::{
    mock_insights, mock_plans, mock_timeline, PlanMeta, PlanStatus,
};

/// 本页面专属样式（按需加载，不再由 main.rs 统一引入）。
const HOME_CSS: Asset = asset!("/assets/home.css");

#[derive(Clone, PartialEq)]
pub enum PageRoute {
    Home,
    Plan(Option<String>), // None = 新建, Some(id) = 编辑已有
    Settings,
}

#[component]
pub fn HomePage(on_navigate: EventHandler<PageRoute>) -> Element {
    let init_status = use_context::<Memo<InitStatus>>();
    let storage = use_context::<Resource<Option<Arc<StorageContext>>>>();

    // ── 弹窗状态 ──
    let mut show_create_modal = use_signal(|| false);

    // ── 计划列表（从 DB 加载，fallback mock） ──
    let mut plans = use_signal(Vec::new);
    let mut plans_loaded = use_signal(|| false);

    // 当 storage 就绪时加载 plans
    use_effect(move || {
        let storage_opt = storage.read().as_ref().and_then(|x| x.as_ref()).cloned();
        if let Some(ctx) = storage_opt {
            if !*plans_loaded.read() {
                plans_loaded.set(true);
                let repo = ctx.plan_repo.clone();
                spawn(async move {
                    match repo.find_all().await {
                        Ok(list) => {
                            let meta_list: Vec<PlanMeta> = list
                                .into_iter()
                                .enumerate()
                                .map(|(i, m)| PlanMeta {
                                    id: m.id,
                                    name: m.name,
                                    description: m.description,
                                    status: PlanStatus::from_str(&m.status),
                                    mode: m.mode,
                                    schedule: None,
                                    strategy: None,
                                    tags: vec![],
                                    created_at: chrono::DateTime::parse_from_rfc3339(&m.created_at)
                                        .map(|d| d.with_timezone(&chrono::Utc))
                                        .unwrap_or_default(),
                                    updated_at: chrono::DateTime::parse_from_rfc3339(&m.updated_at)
                                        .map(|d| d.with_timezone(&chrono::Utc))
                                        .unwrap_or_default(),
                                    orbit_angle: (i as f64 * 72.0) % 360.0,
                                })
                                .collect();
                            plans.set(meta_list);
                        }
                        Err(e) => {
                            tracing::warn!("加载计划列表失败，使用 mock 数据: {}", e);
                            plans.set(mock_plans());
                        }
                    }
                });
            }
        }
    });

    let insights = use_signal(mock_insights);
    let timeline = use_signal(mock_timeline);

    // hover 的计划 id（用于高亮轨道节点）
    let mut hovered_plan = use_signal(|| None::<String>);

    // 状态指示器
    let ai_ready = init_status.read().ai.state == ModuleState::Ready;
    let mcp_ready = init_status.read().mcp.state == ModuleState::Ready;
    let prompt_ready = init_status.read().prompt.state == ModuleState::Ready;

    let all_ready = ai_ready && mcp_ready && prompt_ready;

    // 计划数量统计
    let pending_count = plans
        .read()
        .iter()
        .filter(|p| matches!(p.status, PlanStatus::PendingGeneration))
        .count();
    let generated_count = plans
        .read()
        .iter()
        .filter(|p| matches!(p.status, PlanStatus::Generated))
        .count();

    // ── 弹窗回调 ──
    let handle_modal_confirm = {
        let storage = storage.clone();
        let on_navigate = on_navigate.clone();
        move |data: CreatePlanData| {
            let storage_opt = storage.read().as_ref().and_then(|x| x.as_ref()).cloned();
            if let Some(ctx) = storage_opt {
                let repo = ctx.plan_repo.clone();
                spawn(async move {
                    match repo.create(&data.name, &data.mode).await {
                        Ok(plan_model) => {
                            on_navigate.call(PageRoute::Plan(Some(plan_model.id)));
                        }
                        Err(e) => {
                            tracing::error!("创建计划失败: {}", e);
                        }
                    }
                });
            } else {
                tracing::warn!("Storage 未就绪，无法创建计划");
            }
            show_create_modal.set(false);
        }
    };

    rsx! {
        document::Stylesheet { href: HOME_CSS }
        div { class: "command-center",

            // ── 创建计划弹窗 ──
            CreatePlanModal {
                is_open: show_create_modal(),
                on_close: move |_| show_create_modal.set(false),
                on_confirm: handle_modal_confirm,
            }

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
                    div { class: "cc-stats-overlay",
                        div { class: "cc-stat",
                            span { class: "cc-stat__value", "{pending_count}" }
                            span { class: "cc-stat__label", "待生成" }
                        }
                        div { class: "cc-stat",
                            span { class: "cc-stat__value", "{generated_count}" }
                            span { class: "cc-stat__label", "已生成" }
                        }
                        div { class: "cc-stat",
                            span { class: "cc-stat__value", "{timeline.read().len()}" }
                            span { class: "cc-stat__label", "今日" }
                        }
                    }

                    AiCore {
                        is_active: all_ready,
                        active_plan_count: pending_count,
                    }

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
                    },
                }
            }

            // ═══════════════════════════════════════════════════════
            // 快捷操作栏
            // ═══════════════════════════════════════════════════════
            QuickActionsBar {
                on_new_plan: move |_| show_create_modal.set(true),
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
