//! ExecutionView 组件：竖向时间轴阶段卡片。
//!
//! 每个 WorkflowPhase 对应一个时间轴节点，通过竖线串联：
//! - done：折叠（icon + ✓ + 标题 + 一行摘要）
//! - active：展开（标题 + body 固定 200px + 可选 action 区）
//! - pending：灰显（icon + ○ + 标题）

use dioxus::prelude::*;
use planned_agent::chat::UIAction;
use tracing;

use super::super::components::chat_ui_actions_view::ChatUIActionsView;
use super::super::types::{ExecutionStep, PendingUIState, StepStatus, WorkflowPhase};

// ── 阶段定义 ──────────────────────────────────────────────────────────────────

/// 时间轴阶段条目（不含 conditional，运行时按开关过滤）。
struct PhaseEntry {
    phase: WorkflowPhase,
    icon: &'static str,
    title: &'static str,
}

/// 阶段在时间轴上的显示状态。
#[derive(Clone, Copy, PartialEq)]
enum PhaseStatus {
    Done,
    Active,
    Pending,
}

// ── Props ─────────────────────────────────────────────────────────────────────

#[derive(Props, Clone, PartialEq)]
pub struct ExecutionViewProps {
    /// 当前工作流阶段
    pub phase: WorkflowPhase,
    /// 执行步骤列表（Execute 阶段使用）
    pub steps: Vec<ExecutionStep>,
    /// 待处理的 UI 交互（追问/确认卡片）
    pub pending: Option<PendingUIState>,
    /// 当前阶段的 AI 输出文本（非 Execute 阶段使用）
    pub phase_output: String,
    /// 是否启用输入参数识别
    pub input_params_enabled: bool,
    /// 是否启用输出类型建议
    pub output_params_enabled: bool,
    /// action 回调：(action, choice, pending_state)
    pub on_action: EventHandler<(UIAction, String, PendingUIState)>,
}

// ── 步骤状态辅助 ──────────────────────────────────────────────────────────────

fn status_class(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "workflow-step--pending",
        StepStatus::Running => "workflow-step--running",
        StepStatus::Done => "workflow-step--done",
        StepStatus::Warning => "workflow-step--warning",
        StepStatus::Failed => "workflow-step--failed",
    }
}

fn status_icon(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "○",
        StepStatus::Running => "⬤",
        StepStatus::Done => "✓",
        StepStatus::Warning => "⚠",
        StepStatus::Failed => "✗",
    }
}

/// 将完整数据格式化为可读文本：字符串直接展示，其余按 pretty JSON。
fn pretty_data(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

// ── 组件 ──────────────────────────────────────────────────────────────────────

#[component]
pub fn ExecutionView(props: ExecutionViewProps) -> Element {
    let steps = &props.steps;
    let phase = props.phase;
    let has_steps = !steps.is_empty();
    let is_idle = phase == WorkflowPhase::Idle;

    // Idle + 无步骤 → 占位提示
    if is_idle && !has_steps {
        return rsx! {
            div { class: "workflow-timeline__placeholder",
                "描述你的任务，然后点击「开始执行」"
            }
        };
    }

    // 构建阶段列表（条件阶段按开关过滤）
    let mut phases: Vec<PhaseEntry> = vec![
        PhaseEntry { phase: WorkflowPhase::ClarityCheck, icon: "🔍", title: "需求分析" },
        PhaseEntry { phase: WorkflowPhase::Execute, icon: "⚡", title: "灵活执行" },
    ];
    if props.input_params_enabled {
        phases.push(PhaseEntry { phase: WorkflowPhase::ParamIdentify, icon: "🔧", title: "参数识别" });
    }
    if props.output_params_enabled {
        phases.push(PhaseEntry { phase: WorkflowPhase::OutputSuggesting, icon: "📤", title: "输出类型" });
    }
    phases.push(PhaseEntry { phase: WorkflowPhase::TraceExtracting, icon: "📝", title: "轨迹提取" });
    phases.push(PhaseEntry { phase: WorkflowPhase::Solidifying, icon: "💾", title: "固化计划" });

    // 解析"有效当前阶段"：AwaitingUserAction 回退到触发阶段
    let effective_phase = if phase == WorkflowPhase::AwaitingUserAction {
        props.pending.as_ref().map(|p| p.trigger_phase).unwrap_or(WorkflowPhase::Idle)
    } else {
        phase
    };

    // 找到 effective_phase 在列表中的位置
    let active_idx = phases.iter().position(|e| e.phase == effective_phase);

    // 判断每个阶段的显示状态
    let get_status = |entry_phase: WorkflowPhase| -> PhaseStatus {
        // 全部完成（Idle + 有步骤 = 流程已结束）
        if is_idle {
            return PhaseStatus::Done;
        }
        // 当前阶段
        if entry_phase == effective_phase {
            return PhaseStatus::Active;
        }
        // 根据位置判断前后
        let entry_idx = phases.iter().position(|e| e.phase == entry_phase).unwrap_or(0);
        match active_idx {
            Some(ai) if entry_idx < ai => PhaseStatus::Done,
            _ => PhaseStatus::Pending,
        }
    };

    // 已完成阶段的摘要文本
    let done_summary = |entry: &PhaseEntry| -> String {
        match entry.phase {
            WorkflowPhase::Execute if has_steps => {
                format!("{} 个步骤已完成", steps.len())
            }
            WorkflowPhase::ClarityCheck => "需求明确".to_string(),
            WorkflowPhase::ParamIdentify => "已识别参数".to_string(),
            WorkflowPhase::OutputSuggesting => "已确认输出".to_string(),
            WorkflowPhase::TraceExtracting => "已提取轨迹".to_string(),
            WorkflowPhase::Solidifying => "已固化计划".to_string(),
            _ => String::new(),
        }
    };

    // 是否是"等待用户操作"状态（当前阶段 active + 有 pending）
    let is_awaiting = phase == WorkflowPhase::AwaitingUserAction;

    rsx! {
        div { class: "workflow-timeline",
            for entry in &phases {
                {
                    let status = get_status(entry.phase);
                    let is_active = status == PhaseStatus::Active;
                    let is_last = entry.phase == phases.last().map(|e| e.phase).unwrap_or(WorkflowPhase::Idle);
                    let node_class = match status {
                        PhaseStatus::Done => "workflow-timeline__node workflow-timeline__node--done",
                        PhaseStatus::Active => "workflow-timeline__node workflow-timeline__node--active",
                        PhaseStatus::Pending => "workflow-timeline__node workflow-timeline__node--pending",
                    };

                    // 状态标记
                    let dot = match status {
                        PhaseStatus::Done => "✓",
                        PhaseStatus::Active if is_awaiting => "⏳",
                        PhaseStatus::Active => "⬤",
                        PhaseStatus::Pending => "○",
                    };

                    rsx! {
                        div { class: "{node_class}",
                            // ── 连接线（非最后一个节点） ──
                            if !is_last {
                                div { class: "workflow-timeline__connector" }
                            }

                            // ── Header ──
                            div { class: "workflow-timeline__header",
                                span { class: "workflow-timeline__dot", "{dot}" }
                                span { class: "workflow-timeline__icon", "{entry.icon}" }
                                span { class: "workflow-timeline__title", "{entry.title}" }
                                if status == PhaseStatus::Done {
                                    span { class: "workflow-timeline__summary",
                                        "{done_summary(entry)}"
                                    }
                                }
                            }

                            // ── Body（仅 active 展开） ──
                            if is_active {
                                div { class: "workflow-timeline__body",
                                    // Execute 阶段：步骤列表
                                    if entry.phase == WorkflowPhase::Execute {
                                        if steps.is_empty() {
                                            div { class: "workflow-timeline__spinner-wrap",
                                                span { class: "workflow-timeline__spinner" }
                                                " 准备执行..."
                                            }
                                        }
                                        for step in steps {
                                            {
                                                let duration_str = step.duration_ms
                                                    .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
                                                    .unwrap_or_else(|| "—".to_string());
                                                rsx! {
                                                    div {
                                                        class: "workflow-step {status_class(&step.status)}",
                                                        div { class: "workflow-step__header",
                                                            span {
                                                                class: "workflow-step__icon",
                                                                "{status_icon(&step.status)}"
                                                            }
                                                            span { class: "workflow-step__index",
                                                                "#{step.index}"
                                                            }
                                                            span { class: "workflow-step__tool",
                                                                "{step.tool_name}"
                                                            }
                                                            span { class: "workflow-step__duration",
                                                                "{duration_str}"
                                                            }
                                                        }
                                                        if !step.params_summary.is_empty() {
                                                            div { class: "workflow-step__params",
                                                                "→ {step.params_summary}"
                                                            }
                                                        }
                                                        if !step.result_summary.is_empty() {
                                                            div { class: "workflow-step__result",
                                                                if matches!(step.status, StepStatus::Warning | StepStatus::Failed) {
                                                                    "! "
                                                                }
                                                                "{step.result_summary}"
                                                            }
                                                        }
                                                        if let Some(ref detail) = step.warning_detail {
                                                            div { class: "workflow-step__warning-detail",
                                                                "  调整: {detail}"
                                                            }
                                                        }
                                                        // ── 完整数据展开区（仅步骤完成且有数据时显示） ──
                                                        if step.params_data.is_some() || step.result_data.is_some() {
                                                            details { class: "workflow-step__expand",
                                                                summary { class: "workflow-step__expand-toggle",
                                                                    "查看完整数据"
                                                                }
                                                                div { class: "workflow-step__expand-body",
                                                                    if let Some(params) = &step.params_data {
                                                                        div { class: "workflow-step__expand-section",
                                                                            div { class: "workflow-step__expand-label",
                                                                                "参数"
                                                                            }
                                                                            pre { class: "workflow-step__expand-pre",
                                                                                "{pretty_data(params)}"
                                                                            }
                                                                        }
                                                                    }
                                                                    if let Some(result) = &step.result_data {
                                                                        div { class: "workflow-step__expand-section",
                                                                            div { class: "workflow-step__expand-label",
                                                                                "输出"
                                                                            }
                                                                            pre { class: "workflow-step__expand-pre",
                                                                                "{pretty_data(result)}"
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
                                    } else if matches!(entry.phase, WorkflowPhase::TraceExtracting | WorkflowPhase::Solidifying) {
                                        // 纯 spinner 阶段
                                        div { class: "workflow-timeline__spinner-wrap",
                                            span { class: "workflow-timeline__spinner" }
                                            if entry.phase == WorkflowPhase::TraceExtracting {
                                                " 正在从对话中提取执行轨迹..."
                                            } else {
                                                " 正在提炼执行计划..."
                                            }
                                        }
                                    } else {
                                        // 其他阶段：AI 输出文本
                                        if props.phase_output.is_empty() {
                                            div { class: "workflow-timeline__spinner-wrap",
                                                span { class: "workflow-timeline__spinner" }
                                                " 分析中..."
                                            }
                                        } else {
                                            div { class: "workflow-timeline__text",
                                                "{props.phase_output}"
                                            }
                                        }
                                    }
                                }

                                // ── Action 区（有 pending 时显示） ──
                                if let Some(ref p) = props.pending {
                                    {
                                        let p_clone = p.clone();
                                        let on_action = props.on_action;
                                        rsx! {
                                            div { class: "workflow-timeline__actions",
                                                ChatUIActionsView {
                                                    message: p_clone.message.clone(),
                                                    actions: p_clone.actions.clone(),
                                                    on_action: move |(action, choice): (UIAction, String)| {
                                                        // 打印原始 action / choice UI action 选择结构
                                                        tracing::debug!(
                                                            action_id = %action.id,
                                                            action_type = ?action.action_type,
                                                            label = %action.label,
                                                            choice = %choice,
                                                            "UI action 回调原始入参"
                                                        );

                                                        on_action.call((action, choice, p_clone.clone()));
                                                    },
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
}
