//! FlexibleWorkflow 组件：灵活模式工作流状态机编排 + 三段式布局。
//!
//! 三段布局（从上到下）：
//! ① ContextHeader — 历史上下文（仅 v>0 时显示）
//! ② 执行流程区（flex-grow）— ExecutionView + ChatUIActionsView
//! ③ RequirementInput — 固定底部输入区

use std::sync::Arc;

use dioxus::prelude::*;
use planned_agent::{ChatEvent, ChatService};
use planned_agent_core::types::{Message, MessageContent, MessageRole, UIAction};
use planned_agent_prompt_manager::FilePromptManager;

use crate::services::chat_service::ChatServiceSignal;
use crate::storage::repository::{MessageRepo, PlanFlexibleRepo, PlanRepo};
use crate::context::StorageContext;

use super::context_header::ContextHeader;
use super::execution_view::ExecutionView;
use super::requirement_input::RequirementInput;
use super::super::components::chat_ui_actions_view::ChatUIActionsView;
use super::super::shared::load_plan_data::build_context_string;
use super::super::shared::save_flexible_plan::save_flexible_plan;
use super::super::types::{
    ExecutionStep, PendingUIState, PlanState, StepStatus, WorkflowPhase, WorkflowState,
};

/// 灵活模式三段式布局样式（仅本组件渲染时按需加载）。
const FLEXIBLE_CSS: Asset = asset!("/assets/plan-flexible.css");

#[derive(Props, Clone, PartialEq)]
pub struct FlexibleWorkflowProps {
    pub plan_id: String,
    pub chat_signal: ChatServiceSignal,
    pub plan: PlanState,
    pub workflow: WorkflowState,
    pub storage: Resource<Option<Arc<StorageContext>>>,
}

/// 启动灵活模式工作流：清晰度检查 → 执行 → 固化。
pub fn start_workflow(
    chat_signal: ChatServiceSignal,
    mut workflow: WorkflowState,
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    plan: PlanState,
) {
    let context_snapshot = workflow.context_snapshot.read().clone();
    let requirement = workflow.requirement_text.read().clone();

    if requirement.trim().is_empty() {
        return;
    }

    workflow.set_phase(WorkflowPhase::ClarityChecking);

    spawn(async move {
        let chat_svc = match (*chat_signal.read()).clone() {
            Some(svc) => svc,
            None => {
                workflow.set_phase(WorkflowPhase::Idle);
                return;
            }
        };

        // ── Phase 1：清晰度检查 ──
        let context_str = context_snapshot
            .as_ref()
            .map(|s| build_context_string(s))
            .unwrap_or_default();

        let clarity_svc = chat_svc
            .with_allowed_tools(Some(vec!["request_user_action".to_string()]))
            .with_system_prompt_template(Some("chat/flexible_clarity".to_string()))
            .with_context(if context_str.is_empty() { None } else { Some(context_str.clone()) });

        // 构建 history：system prompt 自动注入，只需 user 消息
        let history = vec![Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text {
                text: requirement.clone(),
            }),
            ..Default::default()
        }];

        let phase1_result = match clarity_svc.chat_with_callback(history, |_| {}).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("FlexibleWorkflow: Phase 1 失败: {}", e);
                workflow.set_phase(WorkflowPhase::Idle);
                return;
            }
        };

        if phase1_result.cancelled {
            workflow.set_phase(WorkflowPhase::Idle);
            return;
        }

        // 剥离 clarity system prompt
        let mut phase2_ready = phase1_result.history;
        if !phase2_ready.is_empty() && matches!(phase2_ready[0].role, MessageRole::System) {
            phase2_ready.remove(0);
        }

        // 处理 Phase 1 结果
        if !phase1_result.pending_ui_actions.is_empty() {
            // 需求不明确 → 等待用户操作
            let pending = &phase1_result.pending_ui_actions[0];
            workflow.set_phase(WorkflowPhase::AwaitingUserAction);
            workflow.set_pending(PendingUIState {
                message: pending.message.clone(),
                actions: pending.actions.clone(),
                history_snapshot: phase2_ready,
            });
            return;
        }

        // 需求明确 → Phase 2 执行
        run_execution_phase(
            chat_svc,
            workflow,
            plan_id,
            storage,
            plan,
            phase2_ready,
            context_str,
        )
        .await;
    });
}

/// Phase 2：灵活执行 + 固化。
async fn run_execution_phase(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut workflow: WorkflowState,
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    mut plan: PlanState,
    history: Vec<Message>,
    context_str: String,
) {
    workflow.set_phase(WorkflowPhase::Executing);

    let exec_svc = chat_svc
        .with_system_prompt_template(Some("chat/flexible_system".to_string()))
        .with_context(if context_str.is_empty() { None } else { Some(context_str) });

    let mut step_counter = 0usize;
    let mut collected_text = String::new();

    let result = exec_svc
        .chat_with_callback(history, |event| {
            match event {
                ChatEvent::ToolCallStart { name, .. } => {
                    step_counter += 1;
                    workflow.push_step(ExecutionStep {
                        index: step_counter,
                        tool_name: name,
                        params_summary: String::new(),
                        result_summary: String::new(),
                        status: StepStatus::Running,
                        warning_detail: None,
                        duration_ms: None,
                    });
                }
                ChatEvent::ToolCallArgsDelta { delta, .. } => {
                    // 更新最后一步的参数摘要
                    workflow.execution_steps.with_mut(|steps| {
                        if let Some(last) = steps.last_mut() {
                            if last.params_summary.len() < 80 {
                                last.params_summary.push_str(&delta);
                            }
                        }
                    });
                }
                ChatEvent::ToolExecuted { name, is_error, content, .. } => {
                    let result_str = match &content {
                        serde_json::Value::String(s) => {
                            let truncated: String =
                                s.chars().take(80).collect();
                            if s.len() > 80 {
                                format!("{}...", truncated)
                            } else {
                                truncated
                            }
                        }
                        other => {
                            let s = serde_json::to_string(other).unwrap_or_default();
                            let truncated: String =
                                s.chars().take(80).collect();
                            truncated
                        }
                    };
                    let status = if is_error {
                        StepStatus::Failed
                    } else {
                        StepStatus::Done
                    };
                    workflow.update_last_step(status, &result_str, None);
                }
                ChatEvent::TextDelta(chunk) => {
                    collected_text.push_str(&chunk);
                }
                ChatEvent::UIActionRequest { message, actions } => {
                    // 执行完成后的确认卡片（确认生成 / 还需补充）
                    workflow.set_pending(PendingUIState {
                        message,
                        actions,
                        history_snapshot: vec![], // 此处不再需要 history
                    });
                }
                _ => {}
            }
        })
        .await;

    match result {
        Ok(response) if !response.cancelled => {
            // 进入固化阶段
            workflow.set_phase(WorkflowPhase::Solidifying);

            // 如果还有 pending UI（确认生成卡片），交给用户操作
            if workflow.pending_ui.read().is_some() {
                // 待用户点击"确认生成"后触发保存
                return;
            }

            // 无 pending UI → 直接保存（自动确认场景）
            trigger_save(Arc::new(exec_svc), workflow, plan_id, storage, plan, collected_text).await;
        }
        Ok(_) => {
            // cancelled
            workflow.set_phase(WorkflowPhase::Idle);
        }
        Err(e) => {
            tracing::error!("FlexibleWorkflow: Phase 2 失败: {}", e);
            workflow.set_phase(WorkflowPhase::Idle);
        }
    }
}

/// 处理用户 UI 操作（追问卡片 / 确认生成卡片）。
pub fn handle_user_action(
    action: UIAction,
    choice: String,
    pending: PendingUIState,
    mut workflow: WorkflowState,
    chat_signal: ChatServiceSignal,
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    plan: PlanState,
) {
    let phase = *workflow.phase.read();

    if action.id == "generate" {
        // 确认生成 → 触发保存
        workflow.set_phase(WorkflowPhase::Solidifying);
        workflow.clear_pending();

        // 收集执行总结文本
        let summary = build_summary_from_steps(&workflow.execution_steps.read());

        let chat_svc = match (*chat_signal.read()).clone() {
            Some(svc) => svc,
            None => {
                workflow.set_phase(WorkflowPhase::Idle);
                return;
            }
        };

        spawn(async move {
            trigger_save(chat_svc, workflow, plan_id, storage, plan, summary).await;
        });
        return;
    }

    // 其他动作（追问回复 / 还需补充）→ 继续执行
    workflow.clear_pending();

    let mut history = pending.history_snapshot;
    history.push(Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text {
            text: choice.clone(),
        }),
        ..Default::default()
    });

    let chat_svc = match (*chat_signal.read()).clone() {
        Some(svc) => svc,
        None => {
            workflow.set_phase(WorkflowPhase::Idle);
            return;
        }
    };

    let context_str = workflow
        .context_snapshot
        .read()
        .as_ref()
        .map(|s| build_context_string(s))
        .unwrap_or_default();

    let exec_svc = chat_svc
        .with_system_prompt_template(Some("chat/flexible_system".to_string()))
        .with_context(if context_str.is_empty() { None } else { Some(context_str.clone()) });

    spawn(async move {
        run_execution_phase(
            Arc::new(exec_svc),
            workflow,
            plan_id,
            storage,
            plan,
            history,
            context_str,
        )
        .await;
    });
}

/// 触发保存：提炼 CoarseGrainedPlan + 写入 plans_flexible。
async fn trigger_save(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut workflow: WorkflowState,
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    mut plan: PlanState,
    summary: String,
) {
    let storage_opt = storage.read().as_ref().and_then(|x| x.as_ref()).cloned();
    let Some(ctx) = storage_opt else {
        tracing::error!("trigger_save: StorageContext 不可用");
        workflow.set_phase(WorkflowPhase::Idle);
        return;
    };

    // 收集 params（从当前 param_values 构造 ParamDef 列表）
    let param_values = workflow.param_values.read().clone();
    let params: Vec<super::super::types::ParamDef> = param_values
        .into_iter()
        .map(|(name, example)| super::super::types::ParamDef {
            name,
            description: String::new(),
            example,
        })
        .collect();

    save_flexible_plan(
        chat_svc,
        plan_id,
        summary,
        params,
        ctx.plan_repo.clone(),
        ctx.plan_flexible_repo.clone(),
        plan,
        workflow,
    )
    .await;
}

/// 从执行步骤构建简化的执行总结文本。
fn build_summary_from_steps(steps: &[ExecutionStep]) -> String {
    let mut summary = String::from("## 执行轨迹回顾\n\n");
    for step in steps {
        let status_mark = match step.status {
            StepStatus::Done => "✓",
            StepStatus::Failed => "✗",
            StepStatus::Warning => "⚠",
            _ => "○",
        };
        summary.push_str(&format!(
            "{}. {} {} → {}\n",
            step.index, status_mark, step.tool_name, step.result_summary
        ));
        if let Some(ref detail) = step.warning_detail {
            summary.push_str(&format!("   - 意外：{}\n", detail));
        }
    }
    summary
}

#[component]
pub fn FlexibleWorkflow(props: FlexibleWorkflowProps) -> Element {
    let snapshot = props.workflow.context_snapshot.read().clone();
    let phase = *props.workflow.phase.read();
    let steps = props.workflow.execution_steps.read().clone();
    let pending = props.workflow.pending_ui.read().clone();

    rsx! {
        document::Stylesheet { href: FLEXIBLE_CSS }
        div { class: "flexible-workflow",
            // ① ContextHeader（仅 v>0 时渲染）
            ContextHeader {
                snapshot: snapshot.clone(),
            }

            // ② 执行流程区（flex-grow）
            div { class: "flexible-workflow__body",
                // 执行步骤展示
                ExecutionView {
                    steps: steps.clone(),
                    phase,
                }

                // 追问/确认卡片
                if let Some(ref p) = pending {
                    {
                        let p_clone = p.clone();
                        let pid = props.plan_id.clone();
                        let chat_sig = props.chat_signal;
                        let storage = props.storage.clone();
                        let plan = props.plan;
                        let wf = props.workflow;
                        rsx! {
                            ChatUIActionsView {
                                message: p_clone.message.clone(),
                                actions: p_clone.actions.clone(),
                                on_action: move |(action, choice)| {
                                    handle_user_action(
                                        action,
                                        choice,
                                        p_clone.clone(),
                                        wf,
                                        chat_sig,
                                        pid.clone(),
                                        storage.clone(),
                                        plan,
                                    );
                                },
                            }
                        }
                    }
                }
            }

            // ③ RequirementInput（固定底部）
            RequirementInput {
                workflow: props.workflow,
                snapshot: snapshot.clone(),
                on_start: {
                    let chat_sig = props.chat_signal;
                    let wf = props.workflow;
                    let pid = props.plan_id.clone();
                    let storage = props.storage.clone();
                    let plan = props.plan;
                    move |_| {
                        start_workflow(chat_sig, wf, pid.clone(), storage.clone(), plan);
                    }
                },
                on_stop: {
                    let chat_sig = props.chat_signal;
                    move |_| {
                        if let Some(ref svc) = *chat_sig.read() {
                            svc.stop();
                        }
                    }
                },
            }
        }
    }
}
