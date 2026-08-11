//! FlexibleWorkflow 组件：灵活模式工作流状态机编排 + 三段式布局。
//!
//! 流程（多次独立 Agent 对话，按阶段串联，**跨阶段只传提炼结果**）：
//! ① 需求分析（输出整理后需求）→ ③ 执行任务（AI 在最后一条回复直接输出 result_data + key_steps JSON）→
//!    ② [条件] 参数识别（消费需求+key_steps）→ ④ [条件] 输出确认（消费 result_data）→
//!    ⑤ 汇总完整需求（组装需求描述 + 输入参数 + 执行步骤 + 工具路径 + 输出格式）→ ⑥ LlmCoarsePlanner 生成计划
//!
//! 每个阶段内部保留自己的小历史（多轮工具循环、追问/确认/截断恢复）；
//! 阶段间通过 [`FlexibleStageContext`] 传递提炼后的结构化结果，不共享完整对话。
//!
//! 三段布局（从上到下）：
//! ① ContextHeader — 历史上下文（仅 v>0 时显示）
//! ② 执行流程区（flex-grow）— ExecutionView 竖向时间轴（action 内嵌）
//! ③ RequirementInput — 固定底部输入区

use std::sync::Arc;
use std::time::Duration;

use dioxus::prelude::*;
use planned_agent::{ChatEvent, ChatService};
use planned_agent::chat::UIAction;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_prompt_manager::FilePromptManager;

use crate::services::chat_service::ChatServiceSignal;
use crate::storage::repository::{MessageRepo, PlanFlexibleRepo, PlanRepo};
use crate::context::StorageContext;

use super::context_header::ContextHeader;
use super::execution_view::ExecutionView;
use super::requirement_input::RequirementInput;
use super::super::shared::load_plan_data::build_context_string;
use super::super::shared::save_flexible_plan::save_flexible_plan;
use super::super::states::{PlanState, WorkflowState};
use super::super::types::{
    ExecutionStep, FlexibleStageContext, ParamDef, PendingUIState, StepStatus, ToolUsageSummary,
    WorkflowPhase,
};

/// 灵活模式三段式布局样式（仅本组件渲染时按需加载）。
const FLEXIBLE_CSS: Asset = asset!("/assets/plan-flexible.css");

/// 需求分析结论在时间轴上的停留时长（毫秒）：AI 输出结论后先停留展示，
/// 再自动流转到「灵活执行」阶段，避免结论一闪而过。
const CLARITY_DONE_PAUSE_MS: u64 = 1800;

#[derive(Props, Clone, PartialEq)]
pub struct FlexibleWorkflowProps {
    pub plan_id: String,
    pub chat_signal: ChatServiceSignal,
    pub plan: PlanState,
    pub workflow: WorkflowState,
    pub storage: Resource<Option<Arc<StorageContext>>>,
}

// ── 流水线入口 ────────────────────────────────────────────────────────────────

/// 启动灵活模式工作流：构造阶段上下文，进入 ClarityCheck 管道。
pub fn start_workflow(
    chat_signal: ChatServiceSignal,
    mut workflow: WorkflowState,
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    plan: PlanState,
) {
    let requirement = workflow.requirement_text.read().clone();
    if requirement.trim().is_empty() {
        return;
    }

    let input_params_enabled = *workflow.input_params_enabled.read();
    let output_params_enabled = *workflow.output_params_enabled.read();

    // 清空上轮阶段的 AI 输出
    workflow.phase_output.set(String::new());

    // 阶段上下文：需求分析输入 = 用户原始需求
    let ctx = FlexibleStageContext {
        raw_requirement: requirement,
        ..Default::default()
    };

    spawn(async move {
        let chat_svc = match (*chat_signal.read()).clone() {
            Some(svc) => svc,
            None => {
                workflow.set_phase(WorkflowPhase::Idle);
                return;
            }
        };

        run_pipeline(
            chat_svc,
            workflow,
            plan_id,
            storage,
            plan,
            WorkflowPhase::ClarityCheck,
            ctx,
            None,
            input_params_enabled,
            output_params_enabled,
        )
        .await;
    });
}

/// 处理用户 UI 操作（追问卡片 / 确认卡片 / 截断继续），恢复对应阶段。
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
    workflow.clear_pending();

    // 用户点「结束」（max_tool_rounds 截断卡片）→ 真正终止流水线：
    // 不把 "结束" 当普通消息恢复执行，避免流水线继续空转。
    if action.id == "stop" {
        workflow.set_phase(WorkflowPhase::Idle);
        return;
    }

    // 恢复阶段输入上下文（pending 时快照）
    let ctx = pending.stage_input.clone().unwrap_or_default();
    // 该阶段小历史（含已注入指令）+ 用户回答
    let mut resume_history = pending.history_snapshot;
    resume_history.push(Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text { text: choice }),
        ..Default::default()
    });

    let trigger_phase = pending.trigger_phase;
    let input_params_enabled = *workflow.input_params_enabled.read();
    let output_params_enabled = *workflow.output_params_enabled.read();

    // 清空上轮阶段的 AI 输出
    workflow.phase_output.set(String::new());

    spawn(async move {
        let chat_svc = match (*chat_signal.read()).clone() {
            Some(svc) => svc,
            None => {
                workflow.set_phase(WorkflowPhase::Idle);
                return;
            }
        };

        run_pipeline(
            chat_svc,
            workflow,
            plan_id,
            storage,
            plan,
            trigger_phase,
            ctx,
            Some(StageResume {
                stage_history: resume_history,
            }),
            input_params_enabled,
            output_params_enabled,
        )
        .await;
    });
}

// ── 流水线核心 ────────────────────────────────────────────────────────────────

/// 挂起恢复数据：该阶段小历史（含已注入指令与用户回答，不含 system）。
struct StageResume {
    stage_history: Vec<Message>,
}

/// 多阶段管道：按 ①→③→②→④→⑤→⑥ 顺序驱动，阶段间传递 [`FlexibleStageContext`]。
///
/// 每个阶段用 `with_allowed_tools` 派生受限工具副本；恢复场景（`resume` 非空）
/// 时从对应阶段继续。
async fn run_pipeline(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut workflow: WorkflowState,
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    plan: PlanState,
    entry_stage: WorkflowPhase,
    mut ctx: FlexibleStageContext,
    resume: Option<StageResume>,
    input_params_enabled: bool,
    output_params_enabled: bool,
) {
    // 阶段隔离：每个阶段的指令通过各自的 system prompt 注入（System 角色——遵循度强、
    // 每次 chat 自动注入、恢复天然防重复）；阶段数据（需求/工具清单/摘要/参数值等）
    // 作为 user 消息传入。需求分析的历史计划上下文也作为 user 消息附加。
    let clarity_svc = chat_svc
        .with_system_prompt_template(Some("flexible/flexible_clarity_check".to_string()))
        .with_allowed_tools(Some(vec![
            "request_user_action".to_string(),
            "builtin_read_documentation".to_string(),
        ]));

    let execute_svc = chat_svc
        .with_system_prompt_template(Some("flexible/flexible_execute".to_string()))
        .with_allowed_tools(None);

    let param_svc = chat_svc
        .with_system_prompt_template(Some("flexible/flexible_param_identify".to_string()))
        .with_allowed_tools(Some(vec![
            "request_user_action".to_string(),
            "builtin_read_documentation".to_string(),
        ]));

    let output_svc = chat_svc
        .with_system_prompt_template(Some("flexible/flexible_output_suggest".to_string()))
        .with_allowed_tools(Some(vec![
            "request_user_action".to_string(),
            "builtin_read_documentation".to_string(),
        ]));

    let mut stage = entry_stage;
    let mut resume_history = resume.map(|r| r.stage_history).unwrap_or_default();

    loop {
        // 阶段切换：清空上一阶段的 AI 输出，避免残留文本串台显示到当前阶段卡片
        // （各阶段 chat 回调可能不流式写文本，若不清理会一直显示上一阶段的 phase_output）
        workflow.phase_output.set(String::new());

        match stage {
            WorkflowPhase::ClarityCheck => {
                stage = run_clarity_check_stage(
                    &clarity_svc,
                    &mut workflow,
                    &mut ctx,
                    std::mem::take(&mut resume_history),
                )
                .await;
            }
            WorkflowPhase::Execute => {
                stage = run_execute_stage(
                    &execute_svc,
                    &mut workflow,
                    &mut ctx,
                    std::mem::take(&mut resume_history),
                )
                .await;
            }
            WorkflowPhase::ParamIdentify => {
                if input_params_enabled {
                    stage = run_param_identify_stage(
                        &param_svc,
                        &mut workflow,
                        &mut ctx,
                        std::mem::take(&mut resume_history),
                    )
                    .await;
                } else {
                    stage = WorkflowPhase::OutputSuggesting;
                }
            }
            WorkflowPhase::OutputSuggesting => {
                if output_params_enabled {
                    stage = run_output_suggest_stage(
                        &output_svc,
                        &mut workflow,
                        &mut ctx,
                        std::mem::take(&mut resume_history),
                    )
                    .await;
                } else {
                    stage = WorkflowPhase::RequirementFinalizing;
                }
            }
            WorkflowPhase::RequirementFinalizing => {
                let ok = assemble_complete_requirement(&mut workflow, &mut ctx);
                if ok {
                    tracing::info!(
                        "灵活模式流水线执行完毕，FlexibleStageContext 全量输出:\n{:#?}",
                        ctx
                    );
                    trigger_save(
                        chat_svc.clone(),
                        workflow,
                        plan_id.clone(),
                        storage.clone(),
                        plan,
                        ctx,
                    )
                    .await;
                }
                break;
            }
            // Idle（挂起等待用户 / 被取消 / 失败）或未知阶段 → 结束本轮
            _ => break,
        }
    }
}

// ── 阶段实现 ──────────────────────────────────────────────────────────────────

/// ① 需求分析阶段。
///
/// 输入 `ctx.raw_requirement`；输出整理后的需求 `ctx.requirement`。
/// 不明确 → request_user_action 追问 → 挂起；明确 → 输出整理后需求 → Execute。
async fn run_clarity_check_stage(
    chat_svc: &ChatService<FilePromptManager>,
    workflow: &mut WorkflowState,
    ctx: &mut FlexibleStageContext,
    resume_history: Vec<Message>,
) -> WorkflowPhase {
    workflow.set_phase(WorkflowPhase::ClarityCheck);

    // 首次进入：构造本阶段小历史（原始需求 + 历史上下文；指令由 system prompt 注入）；
    // 恢复：直接用快照（已含用户回答）
    let history = if resume_history.is_empty() {
        let mut h = vec![user_message(ctx.raw_requirement.clone())];
        // 附加历史计划上下文（快照），供需求分析参考
        if let Some(snapshot) = workflow.context_snapshot.read().as_ref() {
            let context_str = build_context_string(snapshot);
            if !context_str.is_empty() {
                h.push(user_message(format!("## 历史计划上下文\n{}", context_str)));
            }
        }
        h
    } else {
        resume_history
    };

    let pre_len = history.len();
    let pre_first_is_system = history
        .first()
        .map(|m| matches!(m.role, MessageRole::System))
        .unwrap_or(false);

    let result = match chat_svc.chat_with_callback(history, |_| {}).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("需求分析阶段失败: {}", e);
            workflow.set_phase(WorkflowPhase::Idle);
            return WorkflowPhase::Idle;
        }
    };

    if result.cancelled {
        workflow.set_phase(WorkflowPhase::Idle);
        return WorkflowPhase::Idle;
    }

    // 挂起：有追问 → 等待用户操作（快照为本阶段小历史，剥离 system）
    if !result.pending_ui_actions.is_empty() {
        let pa = &result.pending_ui_actions[0];
        workflow.set_phase(WorkflowPhase::AwaitingUserAction);
        workflow.set_pending(PendingUIState {
            message: pa.message.clone(),
            actions: pa.actions.clone(),
            history_snapshot: strip_system_prompt(result.history),
            trigger_phase: WorkflowPhase::ClarityCheck,
            stage_input: Some(ctx.clone()),
        });
        return WorkflowPhase::Idle;
    }

    // 需求明确：提取整理后的需求
    let output = extract_new_assistant_text(&result.history, pre_len, pre_first_is_system);
    if !output.is_empty() {
        ctx.requirement = extract_requirement(&output);
        workflow.set_stage_output(WorkflowPhase::ClarityCheck, output.clone());
        workflow.set_phase_output(output);
        // 停留展示 AI 结论，用户可在此期间点「停止」取消
        tokio::time::sleep(Duration::from_millis(CLARITY_DONE_PAUSE_MS)).await;
        if chat_svc.is_cancelled() {
            workflow.set_phase(WorkflowPhase::Idle);
            return WorkflowPhase::Idle;
        }
    } else {
        tracing::warn!("需求分析阶段：Agent 无文本输出，以原始需求作为整理后需求继续");
        ctx.requirement = ctx.raw_requirement.clone();
    }
    WorkflowPhase::Execute
}

/// ③ 执行任务阶段。
///
/// 输入 `ctx.requirement`（整理后需求）；输出 `ctx.execution_result` + `ctx.tool_usage_summary`
/// （AI 在最后一条回复中直接输出 execution_result + key_steps + tool_steps JSON）。截断/继续执行在本阶段小历史内恢复。
async fn run_execute_stage(
    chat_svc: &ChatService<FilePromptManager>,
    workflow: &mut WorkflowState,
    ctx: &mut FlexibleStageContext,
    resume_history: Vec<Message>,
) -> WorkflowPhase {
    workflow.set_phase(WorkflowPhase::Execute);

    let history = if resume_history.is_empty() {
        let mut h = vec![user_message(ctx.requirement.clone())];
        // 附加本次执行的参数表单值（用户在下一次执行时填写的固化参数）
        let param_values = workflow.param_values.read();
        if !param_values.is_empty() {
            let mut lines = vec!["\n## 本次执行的参数值".to_string()];
            for (name, value) in param_values.iter() {
                lines.push(format!("- {} = {}", name, value));
            }
            h.push(user_message(lines.join("\n")));
        }
        h
    } else {
        resume_history
    };

    let result = match chat_svc
        .chat_with_callback(history, |event| handle_exec_event(event, workflow))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("执行阶段失败: {}", e);
            workflow.set_phase(WorkflowPhase::Idle);
            return WorkflowPhase::Idle;
        }
    };

    if result.cancelled {
        workflow.set_phase(WorkflowPhase::Idle);
        return WorkflowPhase::Idle;
    }

    // 挂起：达到 max_tool_rounds 截断 → 等待用户决定继续/结束
    if !result.pending_ui_actions.is_empty() {
        let pa = &result.pending_ui_actions[0];
        workflow.set_phase(WorkflowPhase::AwaitingUserAction);
        workflow.set_pending(PendingUIState {
            message: pa.message.clone(),
            actions: pa.actions.clone(),
            history_snapshot: strip_system_prompt(result.history),
            trigger_phase: WorkflowPhase::Execute,
            stage_input: Some(ctx.clone()),
        });
        return WorkflowPhase::Idle;
    }

    // 执行完成 → 从 AI 最后一条回复解析 execution_result + key_steps + tool_steps
    //（对应 flexible_execute.toml 的「执行产出要求」JSON 输出）
    let final_text = extract_last_assistant_text(&result.history, 0);
    match parse_execution_output(&final_text) {
        Ok(output) => {
            ctx.execution_result = output.execution_result;
            ctx.tool_usage_summary = ToolUsageSummary {
                key_steps: output.key_steps,
                tool_steps: output.tool_steps,
            };
        }
        Err(e) => {
            tracing::warn!("执行结果 JSON 解析失败（{}），用 final_reply 兜底", e);
            ctx.execution_result = final_text;
            ctx.tool_usage_summary = ToolUsageSummary::default();
        }
    }

    // 记录执行结果作为「灵活执行」卡片的最终结果展示（Done 展开时优先展示）
    if !ctx.execution_result.is_empty() {
        workflow.set_stage_output(WorkflowPhase::Execute, ctx.execution_result.clone());
    }

    WorkflowPhase::ParamIdentify
}

/// ② [条件] 参数识别阶段。
///
/// 输入 `ctx.requirement` + `ctx.tool_usage_summary`（含 key_steps + tool_steps）；输出确认参数 `ctx.confirmed_params`。
async fn run_param_identify_stage(
    chat_svc: &ChatService<FilePromptManager>,
    workflow: &mut WorkflowState,
    ctx: &mut FlexibleStageContext,
    resume_history: Vec<Message>,
) -> WorkflowPhase {
    workflow.set_phase(WorkflowPhase::ParamIdentify);

    let history = if resume_history.is_empty() {
        // 指令由 system prompt（flexible_param_identify）注入；此处只放阶段输入数据
        let tool_info = if !ctx.tool_usage_summary.tool_steps.is_empty() {
            format!(
                "## 执行关键步骤\n{}\n\n## 执行工具步骤\n{}",
                ctx.tool_usage_summary.key_steps.join("\n"),
                ctx.tool_usage_summary.tool_steps.join("\n")
            )
        } else {
            format!(
                "## 执行关键步骤\n{}",
                ctx.tool_usage_summary.key_steps.join("\n")
            )
        };
        vec![user_message(format!(
            "## 整理后的需求\n{}\n\n{}",
            ctx.requirement, tool_info
        ))]
    } else {
        resume_history
    };

    let pre_len = history.len();
    let pre_first_is_system = history
        .first()
        .map(|m| matches!(m.role, MessageRole::System))
        .unwrap_or(false);

    let result = match chat_svc.chat_with_callback(history, |_| {}).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("参数识别阶段失败: {}", e);
            return WorkflowPhase::OutputSuggesting;
        }
    };

    if result.cancelled {
        workflow.set_phase(WorkflowPhase::Idle);
        return WorkflowPhase::Idle;
    }

    // 挂起：Agent 弹参数卡片 → 等待用户勾选/跳过
    if !result.pending_ui_actions.is_empty() {
        let pa = &result.pending_ui_actions[0];
        workflow.set_phase(WorkflowPhase::AwaitingUserAction);
        workflow.set_pending(PendingUIState {
            message: pa.message.clone(),
            actions: pa.actions.clone(),
            history_snapshot: strip_system_prompt(result.history),
            trigger_phase: WorkflowPhase::ParamIdentify,
            stage_input: Some(ctx.clone()),
        });
        return WorkflowPhase::Idle;
    }

    // 完成：从本阶段用户回复（勾选结果）解析确认的参数字段
    ctx.confirmed_params = extract_confirmed_params(&result.history);

    let output = extract_new_assistant_text(&result.history, pre_len, pre_first_is_system);
    if output.is_empty() {
        tracing::warn!("参数识别阶段：Agent 未弹卡片且本阶段无文本输出，直接流转到输出确认");
    } else {
        workflow.set_phase_output(output.clone());
        workflow.set_stage_output(WorkflowPhase::ParamIdentify, output);
    }
    WorkflowPhase::OutputSuggesting
}

/// ④ [条件] 输出确认阶段。
///
/// 输入 `ctx.requirement` + `ctx.execution_result`；输出输出类型 `ctx.output_schema`。
async fn run_output_suggest_stage(
    chat_svc: &ChatService<FilePromptManager>,
    workflow: &mut WorkflowState,
    ctx: &mut FlexibleStageContext,
    resume_history: Vec<Message>,
) -> WorkflowPhase {
    workflow.set_phase(WorkflowPhase::OutputSuggesting);

    let history = if resume_history.is_empty() {
        // 指令由 system prompt（flexible_output_suggest）注入；此处只放阶段输入数据
        vec![user_message(format!(
            "## 用户需求\n{}\n\n## 执行结果数据\n{}",
            ctx.requirement, ctx.execution_result
        ))]
    } else {
        resume_history
    };

    let pre_len = history.len();
    let pre_first_is_system = history
        .first()
        .map(|m| matches!(m.role, MessageRole::System))
        .unwrap_or(false);

    let result = match chat_svc.chat_with_callback(history, |_| {}).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("输出确认阶段失败: {}", e);
            return WorkflowPhase::RequirementFinalizing;
        }
    };

    if result.cancelled {
        workflow.set_phase(WorkflowPhase::Idle);
        return WorkflowPhase::Idle;
    }

    // 挂起：Agent 弹输出类型卡片 → 等待用户确认/跳过
    if !result.pending_ui_actions.is_empty() {
        let pa = &result.pending_ui_actions[0];
        workflow.set_phase(WorkflowPhase::AwaitingUserAction);
        workflow.set_pending(PendingUIState {
            message: pa.message.clone(),
            actions: pa.actions.clone(),
            history_snapshot: strip_system_prompt(result.history),
            trigger_phase: WorkflowPhase::OutputSuggesting,
            stage_input: Some(ctx.clone()),
        });
        return WorkflowPhase::Idle;
    }

    // 完成：记录 Agent 输出的类型建议（展示用；最终 schema 由 Coarse 提炼生成）
    let output = extract_new_assistant_text(&result.history, pre_len, pre_first_is_system);
    if output.is_empty() {
        tracing::warn!("输出确认阶段：Agent 未弹卡片且本阶段无文本输出，直接流转到轨迹提取");
    } else {
        ctx.output_schema = output.clone();
        workflow.set_phase_output(output.clone());
        workflow.set_stage_output(WorkflowPhase::OutputSuggesting, output);
    }
    WorkflowPhase::RequirementFinalizing
}

/// ⑤ 汇总完整需求阶段。
///
/// 输入：ctx 中所有已填充字段；输出 `ctx.complete_requirement`。
/// 纯文本拼接，无需 AI 调用。
fn assemble_complete_requirement(
    workflow: &mut WorkflowState,
    ctx: &mut FlexibleStageContext,
) -> bool {
    workflow.set_phase(WorkflowPhase::RequirementFinalizing);

    let mut text = format!("## 需求描述\n{}\n", ctx.requirement);

    if !ctx.confirmed_params.is_empty() {
        text.push_str("\n## 输入参数\n");
        for p in &ctx.confirmed_params {
            text.push_str(&format!("- {}: {}（示例: {}）\n", p.name, p.description, p.example));
        }
    }

    if !ctx.tool_usage_summary.key_steps.is_empty() {
        text.push_str("\n## 执行步骤\n");
        for s in &ctx.tool_usage_summary.key_steps {
            text.push_str(&format!("- {}\n", s));
        }
    }

    if !ctx.tool_usage_summary.tool_steps.is_empty() {
        text.push_str("\n## 工具路径\n");
        for s in &ctx.tool_usage_summary.tool_steps {
            text.push_str(&format!("- {}\n", s));
        }
    }

    if !ctx.output_schema.is_empty() {
        text.push_str(&format!("\n## 输出格式\n{}\n", ctx.output_schema));
    }

    ctx.complete_requirement = text.clone();
    workflow.set_stage_output(WorkflowPhase::RequirementFinalizing, text);
    true
}

// ── 保存 ──────────────────────────────────────────────────────────────────────

/// 从阶段上下文（完整需求 + 确认参数）生成 CoarseGrainedPlan 并保存到 DB。
async fn trigger_save(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut workflow: WorkflowState,
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    plan: PlanState,
    ctx: FlexibleStageContext,
) {
    workflow.set_phase(WorkflowPhase::Solidifying);

    let storage_opt = storage.read().as_ref().and_then(|x| x.as_ref()).cloned();
    let Some(storage_ctx) = storage_opt else {
        tracing::error!("trigger_save: StorageContext 不可用");
        workflow.set_phase(WorkflowPhase::Idle);
        return;
    };

    save_flexible_plan(
        chat_svc,
        plan_id,
        ctx.complete_requirement,
        ctx.output_schema,
        ctx.confirmed_params,
        storage_ctx.plan_repo.clone(),
        storage_ctx.plan_flexible_repo.clone(),
        plan,
        workflow,
    )
    .await;
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

/// 构造 user role 消息。
fn user_message(text: String) -> Message {
    Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text { text }),
        ..Default::default()
    }
}

/// 剥离 history 开头的 system prompt 消息。
fn strip_system_prompt(mut history: Vec<Message>) -> Vec<Message> {
    if !history.is_empty() && matches!(history[0].role, MessageRole::System) {
        history.remove(0);
    }
    history
}

/// 从 history 中提取最后一条 assistant 消息的文本（仅查找 `from_idx` 之后的消息）。
fn extract_last_assistant_text(history: &[Message], from_idx: usize) -> String {
    history
        .iter()
        .skip(from_idx)
        .rev()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .and_then(|m| match &m.content {
            Some(MessageContent::Text { text }) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// 从 chat 后的完整历史中提取"本次 chat 新增的最后一条 assistant 文本"。
///
/// `pre_len` / `pre_first_is_system` 为 chat 前历史的长度与首条是否 system：
/// `chat_with_callback` 会在首条非 system 时向头部注入 system prompt，导致
/// 后续消息整体后移 1 位，这里用这两个入参做位移修正。
fn extract_new_assistant_text(
    history: &[Message],
    pre_len: usize,
    pre_first_is_system: bool,
) -> String {
    let mut start = pre_len;
    if !pre_first_is_system
        && history.len() > pre_len
        && history
            .first()
            .map(|m| matches!(m.role, MessageRole::System))
            .unwrap_or(false)
    {
        start += 1;
    }
    extract_last_assistant_text(history, start)
}

/// 从需求分析输出中提取"整理后的需求"（`## 整理后的需求` 之后的文本）。
/// 无标记时返回整个输出。
fn extract_requirement(output: &str) -> String {
    const MARK: &str = "## 整理后的需求";
    output
        .find(MARK)
        .map(|idx| output[idx + MARK.len()..].trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| output.to_string())
}

/// 解析用户勾选的参数 choice（"param_xxx=value,param_yyy=value"）为 ParamDef 列表。
///
/// 以 `param_` 前缀为分段边界：value 内若含英文逗号（如 URL query），
/// 不会误切成新段，而是并入前一段。
fn parse_param_choice(choice: &str) -> Vec<ParamDef> {
    let mut segments: Vec<String> = Vec::new();
    for part in choice.split(',') {
        if part.trim_start().starts_with("param_") {
            segments.push(part.trim().to_string());
        } else if let Some(last) = segments.last_mut() {
            last.push(',');
            last.push_str(part);
        }
    }
    segments
        .into_iter()
        .filter_map(|seg| {
            let mut it = seg.splitn(2, '=');
            let id = it.next().unwrap_or("").trim();
            let value = it.next().unwrap_or("").trim();
            if id.starts_with("param_") && !value.is_empty() {
                Some(ParamDef {
                    name: id.trim_start_matches("param_").to_string(),
                    description: String::new(),
                    example: value.to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// 从阶段历史中提取用户确认的参数字段（**最后一条** User 消息中的 `param_xxx=`）。
/// 只取最后一条，避免回退到上一轮勾选值（用户"跳过"时最后一条无 `param_` → 空）。
fn extract_confirmed_params(history: &[Message]) -> Vec<ParamDef> {
    let last_user_text = history.iter().rev().find_map(|m| match &m.content {
        Some(MessageContent::Text { text }) if matches!(m.role, MessageRole::User) => Some(text),
        _ => None,
    });
    match last_user_text {
        Some(text) => parse_param_choice(text),
        None => vec![],
    }
}

/// 执行产出 JSON 解析结构（对应 flexible_execute.toml「执行产出要求」）。
#[derive(serde::Deserialize)]
struct ExecutionOutput {
    execution_result: String,
    key_steps: Vec<String>,
    tool_steps: Vec<String>,
}

/// 从 AI 最后一条回复解析 execution_result + key_steps + tool_steps。
/// 自动去除 markdown 代码块围栏（```json / ```）后再反序列化。
fn parse_execution_output(text: &str) -> Result<ExecutionOutput, String> {
    let json_str = text
        .trim()
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .map(|s| s.strip_suffix("```").unwrap_or(s))
        .unwrap_or(text)
        .trim();
    serde_json::from_str::<ExecutionOutput>(json_str).map_err(|e| format!("JSON 反序列化失败: {}", e))
}

/// 执行阶段的 ChatEvent 处理：构建 ExecutionView 所需的步骤列表。
fn handle_exec_event(event: ChatEvent, workflow: &mut WorkflowState) {
    match event {
        ChatEvent::ToolCallStart { name, .. } => {
            let step_counter = workflow.execution_steps.read().len() + 1;
            workflow.push_step(ExecutionStep {
                index: step_counter,
                tool_name: name,
                params_summary: String::new(),
                result_summary: String::new(),
                status: StepStatus::Running,
                warning_detail: None,
                duration_ms: None,
                params_data: None,
                result_data: None,
            });
        }
        ChatEvent::ToolCallArgsDelta { delta, .. } => {
            workflow.execution_steps.with_mut(|steps| {
                if let Some(last) = steps.last_mut() {
                    if last.params_summary.len() < 80 {
                        last.params_summary.push_str(&delta);
                    }
                }
            });
        }
        ChatEvent::ToolCallComplete { arguments, .. } => {
            // 保存完整参数（不截断），供展开查看
            workflow.execution_steps.with_mut(|steps| {
                if let Some(last) = steps.last_mut() {
                    last.params_data = Some(arguments);
                }
            });
        }
        ChatEvent::ToolExecuted {
            is_error,
            content,
            ..
        } => {
            let result_str = match &content {
                serde_json::Value::String(s) => {
                    let truncated: String = s.chars().take(80).collect();
                    if s.len() > 80 {
                        format!("{}...", truncated)
                    } else {
                        truncated
                    }
                }
                other => {
                    let s = serde_json::to_string(other).unwrap_or_default();
                    let truncated: String = s.chars().take(80).collect();
                    truncated
                }
            };
            let status = if is_error {
                StepStatus::Failed
            } else {
                StepStatus::Done
            };
            // 保留完整输出（不截断），供展开查看
            workflow.execution_steps.with_mut(|steps| {
                if let Some(last) = steps.last_mut() {
                    last.result_data = Some(content.clone());
                }
            });
            workflow.update_last_step(status, &result_str, None);
        }
        _ => {}
    }
}

// ── 组件 ──────────────────────────────────────────────────────────────────────

#[component]
pub fn FlexibleWorkflow(props: FlexibleWorkflowProps) -> Element {
    let snapshot = props.workflow.context_snapshot.read().clone();
    let phase = *props.workflow.phase.read();
    let steps = props.workflow.execution_steps.read().clone();
    let pending = props.workflow.pending_ui.read().clone();
    let phase_output = props.workflow.phase_output.read().clone();
    let stage_outputs = props.workflow.stage_outputs.read().clone();
    let input_params_enabled = *props.workflow.input_params_enabled.read();
    let output_params_enabled = *props.workflow.output_params_enabled.read();

    rsx! {
        document::Stylesheet { href: FLEXIBLE_CSS }
        div { class: "flexible-workflow",
            // ① ContextHeader（仅 v>0 时渲染）
            ContextHeader {
                snapshot: snapshot.clone(),
            }

            // ② 执行流程区（flex-grow）
            div { class: "flexible-workflow__body",
                // 竖向时间轴（action 卡片内嵌在对应阶段卡片中）
                ExecutionView {
                    phase,
                    steps: steps.clone(),
                    pending: pending.clone(),
                    phase_output,
                    stage_outputs,
                    input_params_enabled,
                    output_params_enabled,
                    on_action: {
                        let chat_sig = props.chat_signal;
                        let wf = props.workflow;
                        let pid = props.plan_id.clone();
                        let storage = props.storage.clone();
                        let plan = props.plan;
                        move |(action, choice, p_state)| {
                            handle_user_action(
                                action,
                                choice,
                                p_state,
                                wf,
                                chat_sig,
                                pid.clone(),
                                storage.clone(),
                                plan,
                            );
                        }
                    },
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
