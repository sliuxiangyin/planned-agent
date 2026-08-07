//! 左侧面板容器：组合顶栏与四个 Bento 瓷块，管理「删除计划」弹窗。

use crate::components::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use crate::components::page_header::PageHeader;
use dioxus::prelude::*;

use crate::pages::plan::types::PlanInfo;

use super::dialogs::DeletePlanDialog;
use super::history::HistoryView;
use super::params::ParamsView;
use super::pipeline::PipelineView;
use super::stats::StatsView;

/// 左侧面板样式（按需加载）。
const LEFT_PANEL_CSS: Asset = asset!("/assets/plan-left-panel.css");

#[component]
pub fn PlanLeftPanel(
    plan_id: String,
    on_back: EventHandler<()>,
    plan_info: Signal<Option<PlanInfo>, SyncStorage>,
    on_delete: EventHandler<()>,
) -> Element {
    // ── 删除确认弹窗 ──
    let mut show_delete_dialog = use_signal_sync(|| false);

    // ── 计划元数据派生（模式 / 状态 label 与 chip class） ──
    let plan_name = plan_info
        .read()
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| format!("计划 {}", plan_id));
    let plan_mode_label = plan_info
        .read()
        .as_ref()
        .map(|p| match p.mode.as_str() {
            "thorough" => "周密模式".to_string(),
            _ => "灵活模式".to_string(),
        })
        .unwrap_or_default();
    let plan_status_label = plan_info
        .read()
        .as_ref()
        .map(|p| match p.status.as_str() {
            "generated" => "已生成".to_string(),
            _ => "待生成".to_string(),
        })
        .unwrap_or_default();
    let status_chip_class = plan_info
        .read()
        .as_ref()
        .map(|p| match p.status.as_str() {
            "generated" => "header-chip--status--generated",
            _ => "header-chip--status--pending",
        })
        .unwrap_or("header-chip--status--pending");

    rsx! {
        document::Stylesheet { href: LEFT_PANEL_CSS }
        div { class: "plan-left-panel",
            // ── Header topbar：返回 + 计划名称（PageHeader 组件） ──
            PageHeader {
                title: plan_name.clone(),
                on_back: Some(on_back),
                class: Some("dx-page-header--nested".to_string()),
                actions: Some(rsx! {
                    // ① 模式 chip
                    span {
                        class: "header-chip header-chip--mode",
                        "{plan_mode_label}"
                    }
                    // ② 状态 chip
                    span {
                        class: "header-chip header-chip--status {status_chip_class}",
                        "{plan_status_label}"
                    }
                    // ③ 更多操作下拉菜单
                    DropdownMenu {
                        class: "header-more-menu",
                        DropdownMenuTrigger {
                            class: "header-more-btn",
                            title: "更多操作",
                            svg {
                                xmlns: "http://www.w3.org/2000/svg",
                                width: "16",
                                height: "16",
                                view_box: "0 0 24 24",
                                fill: "currentColor",
                                circle { cx: "12", cy: "12", r: "1.5" }
                                circle { cx: "12", cy: "5", r: "1.5" }
                                circle { cx: "12", cy: "19", r: "1.5" }
                            }
                        }
                        DropdownMenuContent {
                            class: "header-more-menu-content",
                            DropdownMenuItem::<String> {
                                value: "delete".to_string(),
                                index: 0usize,
                                on_select: move |_| show_delete_dialog.set(true),
                                class: "header-dropdown-item--danger",
                                svg {
                                    xmlns: "http://www.w3.org/2000/svg",
                                    width: "14",
                                    height: "14",
                                    view_box: "0 0 24 24",
                                    fill: "none",
                                    stroke: "currentColor",
                                    stroke_width: "2",
                                    stroke_linecap: "round",
                                    stroke_linejoin: "round",
                                    path { d: "M3 6h18" }
                                    path { d: "M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6" }
                                    path { d: "M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2" }
                                }
                                "删除"
                            }
                        }
                    }
                }),
            }
            div { class: "plan-left-panel__divider" }
            // ── Bento 瓷块容器 ──
            div { class: "plan-bento-container",
                // ① PIPELINE — 执行时间线
                PipelineView {}
                // ② + ③ 双列行：PARAMS | STATS
                div { class: "plan-bento-row",
                    ParamsView {}
                    StatsView { plan_mode_label: plan_mode_label }
                }
                // ④ HISTORY — 历史版本
                HistoryView {}
            }
        }

        // ── 删除确认弹窗 ──
        DeletePlanDialog {
            open: show_delete_dialog,
            on_confirm: on_delete,
        }
    }
}
