//! 左侧面板容器：组合顶栏与四个 Bento 瓷块，管理「删除计划」弹窗。

use std::collections::BTreeMap;
use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::flexible::{render_lenient, PlanInput, PlanStep};
use serde_json::Value;

use crate::components::dropdown_menu::{
    DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger,
};
use crate::components::page_header::PageHeader;
use crate::pages::plan::shared::session::SessionManager;
use crate::services::plans_flexible_service::{PlanTemplateState, PlansFlexibleService};
use crate::storage::entities::plan;

use super::dialogs::DeletePlanDialog;
use super::history::HistoryView;
use super::params::ParamsView;
use super::pipeline::PipelineView;
use super::stats::StatsView;

/// 左侧面板样式（按需加载）。
const LEFT_PANEL_CSS: Asset = asset!("/assets/plan-left-panel.css");

/// 已按当前参数值展开的步骤视图（PIPELINE 展示用）。
///
/// 不把 `PlanStep` 直接递进组件：展开是容器层的职责，
/// 组件只负责把「最终要显示什么」画出来。
#[derive(Clone, PartialEq)]
pub(crate) struct RenderedStep {
    /// 结果引用（`#E1`）。
    pub reference: String,
    /// 展开后的子目标；未填的参数保留 `${name}` 原样。
    pub intent: String,
    /// 展开后的期望产出。
    pub expected_output: String,
    /// 本步骤里未填（或缺失）的参数名，供 UI 提示。
    pub missing: Vec<String>,
}

/// 按当前参数值展开模板步骤。
///
/// 用宽容版替换（[`render_lenient`]）：未填的参数保留 `${name}` 原样并回报，
/// 让用户直接看到「还差哪个」；严格版（缺值即报错）留给执行路径。
fn render_steps(steps: &[PlanStep], values: &BTreeMap<String, String>) -> Vec<RenderedStep> {
    steps
        .iter()
        .map(|step| {
            let (intent, mut missing) = render_lenient(&step.intent, values);
            let (expected_output, missing_in_expected) =
                render_lenient(&step.expected_output, values);
            for name in missing_in_expected {
                if !missing.contains(&name) {
                    missing.push(name);
                }
            }
            RenderedStep {
                reference: step.result_reference.clone(),
                intent,
                expected_output,
                missing,
            }
        })
        .collect()
}

/// 生效参数值 = 模板默认值 ⊕ 用户覆盖（覆盖优先）。
///
/// 在**渲染期**合并、而不是只靠 effect 播种：`template_res` 变 `Ready` 的那一帧
/// effect 还没跑（它挂在渲染后），若直接读空表会让输入框全空、PIPELINE 全报未填，闪一帧。
fn effective_param_texts(
    inputs: &[PlanInput],
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    inputs
        .iter()
        .map(|input| {
            let text = overrides
                .get(&input.name)
                .cloned()
                .unwrap_or_else(|| input.default.as_ref().map(value_text).unwrap_or_default());
            (input.name.clone(), text)
        })
        .collect()
}

/// 参数值的文本形式：字符串直出，其余走 JSON 字面量（数字 `1` → `"1"`）。
pub(crate) fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// 把缺失的参数名渲染成 `${a} ${b}`，供 UI 提示。
pub(crate) fn missing_label(names: &[String]) -> String {
    names
        .iter()
        .map(|name| format!("${{{name}}}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// TODO(执行接线)：接执行按钮时，把 UI 的编辑值传给 `FlexibleExecutor`，
// 并让严格版 `render` 与宽容版对空值取一致语义（否则会「UI 报未填、执行静默传空参」）。
#[component]
pub fn PlanLeftPanel(
    plan_id: String,
    on_back: EventHandler<()>,
    plan_info: Option<plan::Model>,
    on_delete: EventHandler<()>,
) -> Element {
    // ── 删除确认弹窗 ──
    let mut show_delete_dialog = use_signal_sync(|| false);

    // ── 当前会话的参数化模板 ──
    // 依赖 (session_id, template_version) 两个信号：切换会话、或 flexible_save 定稿后
    // 由 TemplateNotifier 递增版本，都会自动重读该会话的 `parameterized_task`。
    let session_mgr = use_context::<Arc<SessionManager>>();
    let service = use_context::<Arc<PlansFlexibleService>>();
    let current_session = session_mgr.current();
    let template_version = session_mgr.template_version();
    let template_res = use_resource(move || {
        let session_id = current_session.read().clone();
        let _revision = *template_version.read();
        let service = service.clone();
        async move {
            match session_id {
                Some(sid) => service.load_template(&sid).await,
                None => Ok(PlanTemplateState::NotReady),
            }
        }
    });

    // ── 派生给四个瓷块的视图数据 ──
    // 「未定稿」有两种成因，文案要分开：没选会话 vs 会话还没跑到定稿。
    let not_ready_hint = if current_session.read().is_none() {
        "尚未选择会话"
    } else {
        "尚未生成参数化模板"
    };
    // resource 尚未就绪（None）也当「未就绪」提示，避免首帧先闪一下空态。
    let (inputs, steps, hint) = match template_res.read().as_ref() {
        Some(Ok(PlanTemplateState::Ready(template))) => {
            (template.inputs.clone(), template.steps.clone(), None)
        }
        Some(Ok(PlanTemplateState::NotReady)) => {
            (Vec::new(), Vec::new(), Some(not_ready_hint.to_string()))
        }
        Some(Ok(PlanTemplateState::Invalid(reason))) => (
            Vec::new(),
            Vec::new(),
            Some(format!("模板解析失败: {reason}")),
        ),
        Some(Err(e)) => (Vec::new(), Vec::new(), Some(format!("读取模板失败: {e}"))),
        None => (Vec::new(), Vec::new(), Some("加载中…".to_string())),
    };
    let total_steps = steps.len();

    // ── 参数编辑态：只存「用户覆盖」，未覆盖的在渲染期回落模板默认值 ──
    // 模板变化（换会话 / 重新定稿）时丢弃旧覆盖，免得上一个会话的值落到新模板上。
    // 这里只「写」不「读」param_values，故用户输入不会反过来触发本 effect（不会覆盖正在编辑的值）。
    let mut param_values = use_signal(BTreeMap::<String, String>::new);
    use_effect(move || {
        let _ = template_res.read();
        param_values.set(BTreeMap::new());
    });

    // ── PIPELINE 预览：按当前「生效参数值」展开步骤文本 ──
    // 用宽容版替换：未填的参数保留 `${name}` 原样并回报缺失名。
    // 注意它与执行路径的严格版（`render_lenient` vs `render`）有一处**已知差异**：
    // 空字符串在宽容版里算「未填」，在严格版里却是被成功代入的空值 ——
    // 所以「预览 = 执行」目前只对非空值成立，接线执行按钮时需统一（见下方 TODO）。
    let rendered_steps = {
        let overrides = param_values.read();
        let values = effective_param_texts(&inputs, &overrides);
        render_steps(&steps, &values)
    };

    // ── 计划元数据派生（模式 / 状态 label 与 chip class） ──
    let plan_name = plan_info
        .as_ref()
        .map(|p| p.name.clone())
        .unwrap_or_else(|| format!("计划 {}", plan_id));
    let plan_mode_label = plan_info
        .as_ref()
        .map(|p| match p.mode.as_str() {
            "thorough" => "周密模式".to_string(),
            _ => "灵活模式".to_string(),
        })
        .unwrap_or_default();
    let plan_status_label = plan_info
        .as_ref()
        .map(|p| match p.status.as_str() {
            "generated" => "已生成".to_string(),
            _ => "待生成".to_string(),
        })
        .unwrap_or_default();
    let status_chip_class = plan_info
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
                // ② + ③ 双列行：PARAMS | STATS
                div { class: "plan-bento-row",
                    ParamsView { inputs: inputs.clone(), values: param_values, hint: hint.clone() }
                    StatsView {
                        plan_mode_label: plan_mode_label,
                        total_steps: total_steps,
                        hint: hint.clone(),
                    }
                }
                // ① PIPELINE — 执行时间线（步骤骨架来自当前会话模板）
                PipelineView { steps: rendered_steps.clone(), hint: hint.clone() }

                // ④ HISTORY — 历史执行记录
                HistoryView { hint: hint.clone() }
            }
        }

        // ── 删除确认弹窗 ──
        DeletePlanDialog {
            open: show_delete_dialog,
            on_confirm: on_delete,
        }
    }
}
