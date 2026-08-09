//! FlexibleWorkflow 组件：灵活模式工作流状态机编排 + 三段式布局。
//!
//! 流程（多次独立 Agent 对话，按阶段串联）：
//! ① 清晰度判断（仅 request_user_action 工具）→ ③ 执行任务（全量工具）→
//!    ② [条件] 参数识别 → ④ [条件] 输出类型确认 → ⑤ 提取轨迹 → ⑥ LlmCoarsePlanner 生成计划
//!
//! 三段布局（从上到下）：
//! ① ContextHeader — 历史上下文（仅 v>0 时显示）
//! ② 执行流程区（flex-grow）— ExecutionView 竖向时间轴（action 内嵌）
//! ③ RequirementInput — 固定底部输入区

use std::sync::Arc;
use std::time::Duration;

use dioxus::prelude::*;
use planned_agent::{ChatEvent, ChatService};
use planned_agent_core::prompt::PromptContext;
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
use super::super::types::{ExecutionStep, PendingUIState, StepStatus, WorkflowPhase};

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

/// 启动灵活模式工作流。
///
/// 构建初始 history，进入 Executing 阶段管道循环。
pub fn start_workflow(
    chat_signal: ChatServiceSignal,
    mut workflow: WorkflowState,
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    plan: PlanState,
) {
    let context_str = workflow
        .context_snapshot
        .read()
        .as_ref()
        .map(|s| build_context_string(s))
        .unwrap_or_default();
    let requirement = workflow.requirement_text.read().clone();

    if requirement.trim().is_empty() {
        return;
    }

    let input_params_enabled = *workflow.input_params_enabled.read();
    let output_params_enabled = *workflow.output_params_enabled.read();

    // 清空上轮阶段的 AI 输出
    workflow.phase_output.set(String::new());

    let history = vec![Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text {
            text: requirement,
        }),
        ..Default::default()
    }];

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
            history,
            context_str,
            input_params_enabled,
            output_params_enabled,
        )
        .await;
    });
}

/// 处理用户 UI 操作（追问卡片 / 确认卡片），继续流水线。
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

    let mut history = pending.history_snapshot;
    history.push(Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text { text: choice }),
        ..Default::default()
    });

    let trigger_phase = pending.trigger_phase;
    let input_params_enabled = *workflow.input_params_enabled.read();
    let output_params_enabled = *workflow.output_params_enabled.read();

    // 清空上轮阶段的 AI 输出
    workflow.phase_output.set(String::new());

    let context_str = workflow
        .context_snapshot
        .read()
        .as_ref()
        .map(|s| build_context_string(s))
        .unwrap_or_default();

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
            history,
            context_str,
            input_params_enabled,
            output_params_enabled,
        )
        .await;
    });
}

// ── 流水线核心 ────────────────────────────────────────────────────────────────

/// 多阶段管道：按 ①→③→②→④→⑤→⑥ 顺序驱动 Agent 对话。
///
/// 全程共享一个 ChatService 实例（system prompt 仅注入一次），
/// 按阶段通过 `with_allowed_tools` 派生工具受限的副本。
async fn run_pipeline(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut workflow: WorkflowState,
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    mut plan: PlanState,
    entry_stage: WorkflowPhase,
    history: Vec<Message>,
    context_str: String,
    input_params_enabled: bool,
    output_params_enabled: bool,
) {
    // 基础实例：flexible_system + context，全程复用（system prompt 仅注入一次）
    let base_svc = chat_svc
        .with_system_prompt_template(Some("flexible/flexible_system".to_string()))
        .with_context(if context_str.is_empty() {
            None
        } else {
            Some(context_str)
        });

    // ── Stage ①：清晰度判断（仅 request_user_action + builtin_read_documentation）──
    let clarity_svc = base_svc.with_allowed_tools(Some(vec![
        "request_user_action".to_string(),
        "builtin_read_documentation".to_string(),
    ]));
    let (stage, history) = match entry_stage {
        WorkflowPhase::ClarityCheck => {
            run_clarity_check_stage(&clarity_svc, &mut workflow, history).await
        }
        WorkflowPhase::Execute => {
            (WorkflowPhase::Execute, history)
        }
        WorkflowPhase::ParamIdentify => {
            (WorkflowPhase::ParamIdentify, history)
        }
        WorkflowPhase::OutputSuggesting => {
            // 用户已响应输出建议（确认/跳过）→ 直接进入轨迹提取
            (WorkflowPhase::TraceExtracting, history)
        }
        WorkflowPhase::TraceExtracting => {
            // 直接跳到轨迹提取
            (WorkflowPhase::TraceExtracting, history)
        }
        _ => return,
    };

    // ── Stage ③：执行任务（全量工具）──
    let execute_svc = base_svc.with_allowed_tools(None);
    let (stage, history) = if stage == WorkflowPhase::Execute {
        run_execute_stage(&execute_svc, &mut workflow, history).await
    } else {
        (stage, history)
    };

    // ── Stage ② [条件]：参数识别（仅 request_user_action + builtin_read_documentation）──
    let (stage, history) = if stage == WorkflowPhase::ParamIdentify && input_params_enabled {
        let param_svc = base_svc.with_allowed_tools(Some(vec![
            "request_user_action".to_string(),
            "builtin_read_documentation".to_string(),
        ]));
        run_param_identify_stage(&param_svc, &mut workflow, history).await
    } else if stage == WorkflowPhase::ParamIdentify {
        // input_params_enabled 为 false，跳过该阶段
        (WorkflowPhase::OutputSuggesting, history)
    } else {
        (stage, history)
    };

    // ── Stage ④ [条件]：输出类型建议（仅 request_user_action + builtin_read_documentation）──
    let (stage, history) = if stage == WorkflowPhase::OutputSuggesting && output_params_enabled {
        let output_svc = base_svc.with_allowed_tools(Some(vec![
            "request_user_action".to_string(),
            "builtin_read_documentation".to_string(),
        ]));
        run_output_suggest_stage(&output_svc, &mut workflow, history).await
    } else if stage == WorkflowPhase::OutputSuggesting {
        // output_params_enabled 为 false，跳过该阶段
        (WorkflowPhase::TraceExtracting, history)
    } else {
        (stage, history)
    };

    // ── Stage ⑤：轨迹提取 → ⑥ 保存（零工具）──
    if stage == WorkflowPhase::TraceExtracting {
        let trace_svc = base_svc.with_allowed_tools(Some(vec![]));
        let trace_text = run_trace_extract_stage(
            &trace_svc, &mut workflow, history,
        ).await;

        if let Some(trace) = trace_text {
            trigger_save(
                chat_svc,
                workflow,
                plan_id,
                storage,
                plan,
                trace,
            )
            .await;
        }
    }
}

// ── 阶段实现 ──────────────────────────────────────────────────────────────────

/// ① 清晰度判断阶段。
///
/// 注入 `flexible_clarity_check` message，Agent 根据 message 指令判断需求清晰度。
/// 需求明确 → 进入 Execute；不明确 → request_user_action 追问 → 挂起等待用户操作。
async fn run_clarity_check_stage(
    chat_svc: &ChatService<FilePromptManager>,
    workflow: &mut WorkflowState,
    mut history: Vec<Message>,
) -> (WorkflowPhase, Vec<Message>) {
    workflow.set_phase(WorkflowPhase::ClarityCheck);

    // 首次进入时注入清晰度判断消息（防重复注入）
    if !has_clarity_check_message(&history) {
        if let Some(msg) =
            render_message(chat_svc, "flexible/flexible_clarity_check", &PromptContext::new()).await
        {
            history.push(user_message(msg));
        }
    }

    let result = match chat_svc.chat_with_callback(history, |_| {}).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("清晰度判断阶段失败: {}", e);
            workflow.set_phase(WorkflowPhase::Idle);
            return (WorkflowPhase::Idle, vec![]);
        }
    };

    if result.cancelled {
        workflow.set_phase(WorkflowPhase::Idle);
        return (WorkflowPhase::Idle, vec![]);
    }

    // 挂起：有追问 → 等待用户操作（快照需剥离 system prompt）
    if !result.pending_ui_actions.is_empty() {
        let pa = &result.pending_ui_actions[0];
        let clean_history = strip_system_prompt(result.history);
        workflow.set_phase(WorkflowPhase::AwaitingUserAction);
        workflow.set_pending(PendingUIState {
            message: pa.message.clone(),
            actions: pa.actions.clone(),
            history_snapshot: clean_history,
            trigger_phase: WorkflowPhase::ClarityCheck,
        });
        return (WorkflowPhase::Idle, vec![]);
    }

    // 需求明确 → 进入执行阶段（保留 system prompt，后续阶段复用）
    let output = extract_last_assistant_text(&result.history);
    if !output.is_empty() {
        workflow.set_phase_output(output);
        // 停留展示 AI 结论（ClarityCheck 保持 active，ExecutionView 渲染 phase_output），
        // 用户可在此时点「停止」取消；停留结束且未被取消再自动进入执行。
        tokio::time::sleep(Duration::from_millis(CLARITY_DONE_PAUSE_MS)).await;
        if chat_svc.is_cancelled() {
            workflow.set_phase(WorkflowPhase::Idle);
            return (WorkflowPhase::Idle, vec![]);
        }
    }
    (WorkflowPhase::Execute, result.history)
}

/// ③ 执行任务阶段。
///
/// 全量工具，实时展示执行步骤。遇 pending 则挂起等待用户操作。
async fn run_execute_stage(
    chat_svc: &ChatService<FilePromptManager>,
    workflow: &mut WorkflowState,
    history: Vec<Message>,
) -> (WorkflowPhase, Vec<Message>) {
    workflow.set_phase(WorkflowPhase::Execute);

    let result = match chat_svc
        .chat_with_callback(history, |event| handle_exec_event(event, workflow))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("执行阶段失败: {}", e);
            workflow.set_phase(WorkflowPhase::Idle);
            return (WorkflowPhase::Idle, vec![]);
        }
    };

    if result.cancelled {
        workflow.set_phase(WorkflowPhase::Idle);
        return (WorkflowPhase::Idle, vec![]);
    }

    // 挂起：有 pending action → 等待用户操作（快照需剥离 system prompt）
    if !result.pending_ui_actions.is_empty() {
        let pa = &result.pending_ui_actions[0];
        let clean_history = strip_system_prompt(result.history);
        workflow.set_phase(WorkflowPhase::AwaitingUserAction);
        workflow.set_pending(PendingUIState {
            message: pa.message.clone(),
            actions: pa.actions.clone(),
            history_snapshot: clean_history,
            trigger_phase: WorkflowPhase::Execute,
        });
        return (WorkflowPhase::Idle, vec![]);
    }

    // 执行完成 → 进入参数识别阶段（保留 system prompt，后续阶段复用）
    (WorkflowPhase::ParamIdentify, result.history)
}

/// ② [条件] 参数识别阶段。
///
/// 注入 `flexible_param_identify` message，识别可参数化的动态值。
/// 遇 pending 则挂起等待用户操作；跳过/确认后进入 OutputSuggesting。
async fn run_param_identify_stage(
    chat_svc: &ChatService<FilePromptManager>,
    workflow: &mut WorkflowState,
    mut history: Vec<Message>,
) -> (WorkflowPhase, Vec<Message>) {
    workflow.set_phase(WorkflowPhase::ParamIdentify);

    // 首次进入时注入参数识别消息（防重复注入）
    if !has_param_identify_message(&history) {
        if let Some(msg) =
            render_message(chat_svc, "flexible/flexible_param_identify", &PromptContext::new()).await
        {
            history.push(user_message(msg));
        }
    }

    let result = match chat_svc.chat_with_callback(history, |_| {}).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("参数识别阶段失败: {}", e);
            // 失败不阻塞流程，跳过本阶段
            return (WorkflowPhase::OutputSuggesting, vec![]);
        }
    };

    if result.cancelled {
        workflow.set_phase(WorkflowPhase::Idle);
        return (WorkflowPhase::Idle, vec![]);
    }

    // 挂起：有 pending action → 等待用户操作（快照需剥离 system prompt）
    if !result.pending_ui_actions.is_empty() {
        let pa = &result.pending_ui_actions[0];
        let clean_history = strip_system_prompt(result.history);
        workflow.set_phase(WorkflowPhase::AwaitingUserAction);
        workflow.set_pending(PendingUIState {
            message: pa.message.clone(),
            actions: pa.actions.clone(),
            history_snapshot: clean_history,
            trigger_phase: WorkflowPhase::ParamIdentify,
        });
        return (WorkflowPhase::Idle, vec![]);
    }

    // 参数识别完成 → 进入输出类型建议阶段（保留 system prompt，后续阶段复用）
    let output = extract_last_assistant_text(&result.history);
    if !output.is_empty() {
        workflow.set_phase_output(output);
    }
    (WorkflowPhase::OutputSuggesting, result.history)
}

/// 输出类型建议阶段。
async fn run_output_suggest_stage(
    chat_svc: &ChatService<FilePromptManager>,
    workflow: &mut WorkflowState,
    mut history: Vec<Message>,
) -> (WorkflowPhase, Vec<Message>) {
    workflow.set_phase(WorkflowPhase::OutputSuggesting);

    let Some(msg) =
        render_message(chat_svc, "flexible/flexible_output_suggest", &PromptContext::new()).await
    else {
        // 渲染失败 → 跳过本阶段
        return (WorkflowPhase::TraceExtracting, history);
    };
    history.push(user_message(msg));

    let result = match chat_svc.chat_with_callback(history, |_| {}).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("输出建议阶段失败: {}", e);
            return (WorkflowPhase::TraceExtracting, vec![]);
        }
    };

    if result.cancelled {
        workflow.set_phase(WorkflowPhase::Idle);
        return (WorkflowPhase::Idle, vec![]);
    }

    // 挂起：有 pending action → 等待用户操作（快照需剥离 system prompt）
    if !result.pending_ui_actions.is_empty() {
        let pa = &result.pending_ui_actions[0];
        let clean_history = strip_system_prompt(result.history);
        workflow.set_phase(WorkflowPhase::AwaitingUserAction);
        workflow.set_pending(PendingUIState {
            message: pa.message.clone(),
            actions: pa.actions.clone(),
            history_snapshot: clean_history,
            trigger_phase: WorkflowPhase::OutputSuggesting,
        });
        return (WorkflowPhase::Idle, vec![]);
    }

    // 进入轨迹提取阶段（保留 system prompt，后续阶段复用）
    let output = extract_last_assistant_text(&result.history);
    if !output.is_empty() {
        workflow.set_phase_output(output);
    }
    (WorkflowPhase::TraceExtracting, result.history)
}

/// 轨迹提取阶段 → 返回提取的轨迹文本。
///
/// 返回 `None` 表示阶段失败，调用方应重置工作流。
async fn run_trace_extract_stage(
    chat_svc: &ChatService<FilePromptManager>,
    workflow: &mut WorkflowState,
    mut history: Vec<Message>,
) -> Option<String> {
    workflow.set_phase(WorkflowPhase::TraceExtracting);

    let input_params_enabled = *workflow.input_params_enabled.read();
    let output_params_enabled = *workflow.output_params_enabled.read();
    let param_hints = build_param_hints(workflow);
    let ctx = PromptContext::new()
        .with_variable("param_hints", serde_json::Value::String(param_hints))
        .with_variable(
            "input_params_enabled",
            serde_json::Value::Bool(input_params_enabled),
        )
        .with_variable(
            "output_params_enabled",
            serde_json::Value::Bool(output_params_enabled),
        );

    let Some(msg) =
        render_message(chat_svc, "flexible/flexible_trace_extract", &ctx).await
    else {
        workflow.set_phase(WorkflowPhase::Idle);
        return None;
    };
    history.push(user_message(msg));

    let result = match chat_svc.chat_with_callback(history, |_| {}).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("轨迹提取阶段失败: {}", e);
            workflow.set_phase(WorkflowPhase::Idle);
            return None;
        }
    };

    if result.cancelled {
        workflow.set_phase(WorkflowPhase::Idle);
        return None;
    }

    Some(extract_last_assistant_text(&result.history))
}

// ── 保存 ──────────────────────────────────────────────────────────────────────

/// 从 Agent 提取的轨迹生成 CoarseGrainedPlan 并保存到 DB。
async fn trigger_save(
    chat_svc: Arc<ChatService<FilePromptManager>>,
    mut workflow: WorkflowState,
    plan_id: String,
    storage: Resource<Option<Arc<StorageContext>>>,
    mut plan: PlanState,
    trace_text: String,
) {
    workflow.set_phase(WorkflowPhase::Solidifying);

    let storage_opt = storage.read().as_ref().and_then(|x| x.as_ref()).cloned();
    let Some(ctx) = storage_opt else {
        tracing::error!("trigger_save: StorageContext 不可用");
        workflow.set_phase(WorkflowPhase::Idle);
        return;
    };

    // 收集 params
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
        trace_text,
        params,
        ctx.plan_repo.clone(),
        ctx.plan_flexible_repo.clone(),
        plan,
        workflow,
    )
    .await;
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

/// 通过 ChatService 渲染消息模板。
async fn render_message(
    chat_svc: &ChatService<FilePromptManager>,
    template: &str,
    ctx: &PromptContext,
) -> Option<String> {
    match chat_svc.render_message_template(template, ctx).await {
        Ok(text) if !text.is_empty() => Some(text),
        Ok(_) => None,
        Err(e) => {
            tracing::error!("渲染消息模板 '{}' 失败: {}", template, e);
            None
        }
    }
}

/// 检查 history 中是否已包含参数识别消息（避免重复注入）。
fn has_param_identify_message(history: &[Message]) -> bool {
    history.iter().any(|m| {
        if let Some(MessageContent::Text { text }) = &m.content {
            text.contains("识别可参数化的动态值")
        } else {
            false
        }
    })
}

/// 检查 history 中是否已包含清晰度判断消息（避免重复注入）。
fn has_clarity_check_message(history: &[Message]) -> bool {
    history.iter().any(|m| {
        if let Some(MessageContent::Text { text }) = &m.content {
            text.contains("当前阶段：清晰度判断")
        } else {
            false
        }
    })
}

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

/// 从 history 中提取最后一条 assistant 消息的文本。
fn extract_last_assistant_text(history: &[Message]) -> String {
    history
        .iter()
        .rev()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .and_then(|m| match &m.content {
            Some(MessageContent::Text { text }) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// 构造轨迹润色用的参数提示。
///
/// 如果用户固化了参数，告知 Agent 在提取轨迹时将具体值替换为 `{{param_name}}` 占位符。
fn build_param_hints(workflow: &WorkflowState) -> String {
    let param_values = workflow.param_values.read();
    if param_values.is_empty() {
        return String::new();
    }

    let mut hints = String::from("\n## 参数占位符替换\n\n");
    hints.push_str("提取轨迹时，请将以下参数的具体值替换为占位符：\n");
    for (name, value) in param_values.iter() {
        hints.push_str(&format!("- `{{{{{}}}}} = \"{}\"`\n", name, value));
    }
    hints
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
