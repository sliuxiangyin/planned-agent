//! ExecutionView 组件：中间滚动区的步骤化执行展示。
//!
//! 替代 MessageListView 的对话气泡，展示为步骤列表：
//! 工具名 + 参数摘要 + 耗时 + 状态图标（✓/⬤/⚠/○）+ 意外调整说明。

use dioxus::prelude::*;

use super::super::types::{ExecutionStep, StepStatus, WorkflowPhase};

#[derive(Props, Clone, PartialEq)]
pub struct ExecutionViewProps {
    /// 执行步骤列表
    pub steps: Vec<ExecutionStep>,
    /// 当前工作流阶段
    pub phase: WorkflowPhase,
}

/// 步骤状态 → CSS class
fn status_class(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "execution-step--pending",
        StepStatus::Running => "execution-step--running",
        StepStatus::Done => "execution-step--done",
        StepStatus::Warning => "execution-step--warning",
        StepStatus::Failed => "execution-step--failed",
    }
}

/// 步骤状态 → 图标
fn status_icon(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "○",
        StepStatus::Running => "⬤",
        StepStatus::Done => "✓",
        StepStatus::Warning => "⚠",
        StepStatus::Failed => "✗",
    }
}

#[component]
pub fn ExecutionView(props: ExecutionViewProps) -> Element {
    let steps = &props.steps;
    let phase = props.phase;

    rsx! {
        div { class: "execution-view",
            // 清晰度检查阶段
            if matches!(phase, WorkflowPhase::ClarityChecking) {
                div { class: "execution-phase",
                    div { class: "execution-phase__header execution-phase__header--active",
                        "Step 1  🔍 清晰度检查"
                    }
                    div { class: "execution-phase__body",
                        span { class: "execution-phase__spinner" }
                        " 正在分析需求..."
                    }
                }
            }

            // 执行阶段
            if matches!(phase, WorkflowPhase::Executing | WorkflowPhase::Solidifying)
                || (matches!(phase, WorkflowPhase::Idle) && !steps.is_empty())
            {
                div { class: "execution-phase",
                    div {
                        class: if matches!(phase, WorkflowPhase::Executing) {
                            "execution-phase__header execution-phase__header--active"
                        } else {
                            "execution-phase__header execution-phase__header--done"
                        },
                        if phase == WorkflowPhase::Solidifying {
                            "Step 2  ⚡ 灵活执行"
                        } else {
                            "Step 2  ⚡ 灵活执行"
                        }
                    }
                    if steps.is_empty() && phase == WorkflowPhase::Executing {
                        div { class: "execution-phase__body",
                            span { class: "execution-phase__spinner" }
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
                                    class: "execution-step {status_class(&step.status)}",
                                    div { class: "execution-step__header",
                                        span {
                                            class: "execution-step__icon",
                                            "{status_icon(&step.status)}"
                                        }
                                        span { class: "execution-step__index",
                                            "#{step.index}"
                                        }
                                        span { class: "execution-step__tool",
                                            "{step.tool_name}"
                                        }
                                        span { class: "execution-step__duration",
                                            "{duration_str}"
                                        }
                                    }
                                    if !step.params_summary.is_empty() {
                                        div { class: "execution-step__params",
                                            "→ {step.params_summary}"
                                        }
                                    }
                                    if !step.result_summary.is_empty() {
                                        div { class: "execution-step__result",
                                            if matches!(step.status, StepStatus::Warning | StepStatus::Failed) {
                                                "! "
                                            }
                                            "{step.result_summary}"
                                        }
                                    }
                                    if let Some(ref detail) = step.warning_detail {
                                        div { class: "execution-step__warning-detail",
                                            "  调整: {detail}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 固化阶段
            if matches!(phase, WorkflowPhase::Solidifying) {
                div { class: "execution-phase",
                    div {
                        class: if steps.is_empty() || steps.last().map(|s| &s.status) == Some(&StepStatus::Done) {
                            "execution-phase__header execution-phase__header--active"
                        } else {
                            "execution-phase__header"
                        },
                        "Step 3  📝 固化计划"
                    }
                    div { class: "execution-phase__body",
                        span { class: "execution-phase__spinner" }
                        " 正在提炼执行计划..."
                    }
                }
            }

            // Idle 且无步骤（首次或刚重置）
            if matches!(phase, WorkflowPhase::Idle) && steps.is_empty() {
                div { class: "execution-view__placeholder",
                    "描述你的任务，然后点击「开始执行」"
                }
            }
        }
    }
}
