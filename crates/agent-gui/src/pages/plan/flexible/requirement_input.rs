//! RequirementInput 组件：固定在底部的需求输入区。
//!
//! Idle 状态：可编辑任务描述 + 参数表单 + [开始执行] 按钮
//! 执行中：禁用输入 + 显示当前阶段标签 + [停止] 按钮

use dioxus::prelude::*;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::textarea::Textarea;

use super::super::states::WorkflowState;
use super::super::types::{PlanFlexibleSnapshot, WorkflowPhase};

#[derive(Props, Clone, PartialEq)]
pub struct RequirementInputProps {
    /// 工作流状态
    pub workflow: WorkflowState,
    /// 历史上下文（用于提取 params 定义渲染参数表单）
    pub snapshot: Option<PlanFlexibleSnapshot>,
    /// 用户点击"开始执行"的回调
    pub on_start: EventHandler<()>,
    /// 用户点击"停止"的回调
    pub on_stop: EventHandler<()>,
}

/// 从 params JSON 解析出参数定义列表（仅 name/description/example）。
fn parse_param_defs(snapshot: &PlanFlexibleSnapshot) -> Vec<(String, String)> {
    if snapshot.params.is_empty() || snapshot.params == "[]" {
        return vec![];
    }
    match serde_json::from_str::<Vec<serde_json::Value>>(&snapshot.params) {
        Ok(list) => list
            .iter()
            .map(|v| {
                let name = v
                    .get("name")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let example = v
                    .get("example")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                (name, example)
            })
            .filter(|(n, _)| !n.is_empty())
            .collect(),
        Err(_) => vec![],
    }
}

#[component]
pub fn RequirementInput(props: RequirementInputProps) -> Element {
    let phase = *props.workflow.phase.read();
    let is_running = props.workflow.is_running();
    let param_defs = props
        .snapshot
        .as_ref()
        .map(|s| parse_param_defs(s))
        .unwrap_or_default();

    // 读取当前参数值
    let param_values = props.workflow.param_values.read();
    // 读取当前需求文本
    let requirement_text = props.workflow.requirement_text.read();

    let phase_label = match phase {
        WorkflowPhase::Idle => "",
        WorkflowPhase::AwaitingUserAction => "⏳ 等待用户操作",
        WorkflowPhase::ClarityCheck => "🔍 分析需求清晰度...",
        WorkflowPhase::Execute => "⚡ 灵活执行中...",
        WorkflowPhase::ParamIdentify => "🏷️ 识别可参数化动态值...",
        WorkflowPhase::OutputSuggesting => "📋 确认输出类型...",
        WorkflowPhase::RequirementFinalizing => "📋 汇总需求...",
        WorkflowPhase::Solidifying => "💾 固化计划中...",
        // 周密模式 Executing 按 Idle 处理（灵活模式不会走到这里）
        WorkflowPhase::Executing => "",
    };

    rsx! {
        div { class: "requirement-input",
            // 参数表单（仅当有固化参数定义且 Idle 时显示）
            if !param_defs.is_empty() && phase == WorkflowPhase::Idle {
                div { class: "requirement-input__params",
                    for (name, example) in &param_defs {
                        {
                            // 在当前 param_values 中查找已填值
                            let current_val = param_values
                                .iter()
                                .find(|(n, _)| n == name)
                                .map(|(_, v)| v.clone())
                                .unwrap_or_else(|| example.clone());
                            let n = name.clone();
                            rsx! {
                                div { class: "requirement-input__param-row",
                                    label {
                                        class: "requirement-input__param-label",
                                        r#for: "param-{name}",
                                        "{name}"
                                    }
                                    input {
                                        class: "requirement-input__param-input",
                                        id: "param-{name}",
                                        value: "{current_val}",
                                        placeholder: "{example}",
                                        disabled: is_running,
                                        oninput: {
                                            let mut wf = props.workflow;
                                            let name_clone = n.clone();
                                            move |e: FormEvent| {
                                                let val = e.value();
                                                wf.param_values.with_mut(|pv| {
                                                    if let Some(existing) = pv.iter_mut().find(|(n, _)| n == &name_clone) {
                                                        existing.1 = val.clone();
                                                    } else {
                                                        pv.push((name_clone.clone(), val));
                                                    }
                                                });
                                            }
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 任务描述 Textarea
            div { class: "requirement-input__textarea-wrap",
                Textarea {
                    class: "requirement-input__textarea".to_string(),
                    placeholder: if is_running {
                        phase_label.to_string()
                    } else {
                        "描述你需要执行的任务...".to_string()
                    },
                    value: "{requirement_text}",
                    disabled: is_running,
                    oninput: {
                        let mut wf = props.workflow;
                        move |e: FormEvent| {
                            wf.requirement_text.set(e.value());
                        }
                    },
                    onkeydown: {
                        let on_start = props.on_start.clone();
                        move |e: KeyboardEvent| {
                            if e.data.key() == keyboard_types::Key::Enter
                                && !e.data.modifiers().shift()
                                && !is_running
                            {
                                e.prevent_default();
                                on_start.call(());
                            }
                        }
                    },
                }
            }

            // 功能开关（仅 Idle 时显示）
            if phase == WorkflowPhase::Idle {
                div { class: "requirement-input__toggles",
                    label { class: "requirement-input__toggle",
                        input {
                            r#type: "checkbox",
                            checked: *props.workflow.input_params_enabled.read(),
                            disabled: is_running,
                            onclick: {
                                let mut wf = props.workflow;
                                move |_| {
                                    let cur = *wf.input_params_enabled.read();
                                    wf.input_params_enabled.set(!cur);
                                }
                            },
                        }
                        span { class: "requirement-input__toggle-label", "输入参数" }
                        span { class: "requirement-input__toggle-hint", "识别可参数化的动态值" }
                    }
                    label { class: "requirement-input__toggle",
                        input {
                            r#type: "checkbox",
                            checked: *props.workflow.output_params_enabled.read(),
                            disabled: is_running,
                            onclick: {
                                let mut wf = props.workflow;
                                move |_| {
                                    let cur = *wf.output_params_enabled.read();
                                    wf.output_params_enabled.set(!cur);
                                }
                            },
                        }
                        span { class: "requirement-input__toggle-label", "输出参数" }
                        span { class: "requirement-input__toggle-hint", "确认返回数据类型" }
                    }
                }
            }

            // 操作按钮行
            div { class: "requirement-input__actions",
                if is_running {
                    span { class: "requirement-input__phase-label", "{phase_label}" }
                    Button {
                        class: "requirement-input__stop-btn".to_string(),
                        variant: ButtonVariant::Destructive,
                        size: ButtonSize::Xs,
                        onclick: move |_: MouseEvent| props.on_stop.call(()),
                        "停止"
                    }
                } else {
                    div { class: "requirement-input__placeholder" }
                    Button {
                        class: "requirement-input__start-btn".to_string(),
                        variant: ButtonVariant::Primary,
                        size: ButtonSize::Xs,
                        disabled: requirement_text.trim().is_empty(),
                        onclick: move |_: MouseEvent| props.on_start.call(()),
                        "开始执行"
                    }
                }
            }
        }
    }
}
