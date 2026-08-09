//! 灵活模式工作流类型：阶段枚举、历史快照、执行步骤。

use serde_json::Value;

/// 灵活模式工作流阶段。
///
/// Agent 在多次独立对话中依次经历：
/// ① 清晰度判断 → ③ 执行任务 → ② [条件] 参数识别 → ④ 输出类型确认 → ⑤ 轨迹提取。
/// 任意阶段都可能因 `request_user_action` 进入 `AwaitingUserAction` 等待用户响应。
///
/// 注意：`Executing` 仅由周密模式（`chat.rs`）使用，灵活模式已拆分为下方三个独立阶段。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum WorkflowPhase {
    /// 等待用户输入任务描述
    Idle,
    /// 周密模式专用：Agent 执行中
    Executing,
    /// 灵活模式 ①：清晰度判断 + 追问（仅 request_user_action 工具）
    ClarityCheck,
    /// 灵活模式 ③：执行任务（全量工具）
    Execute,
    /// 灵活模式 ② [条件]：可参数化动态值识别（输入参数开关控制）
    ParamIdentify,
    /// ④ 输出类型建议与确认中（仅输出参数开启时触发）
    OutputSuggesting,
    /// ⑤ 从对话上下文提取执行轨迹中
    TraceExtracting,
    /// 等待用户回复追问/确认卡片
    AwaitingUserAction,
    /// ⑥ 执行完成，提炼计划 + 保存中
    Solidifying,
}

/// 从 `plans_flexible` 加载的四字段快照，供下次执行注入 context。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PlanFlexibleSnapshot {
    pub version: i64,
    pub todos: String,           // CoarseGrainedPlan JSON
    pub previous_summary: String, // AI 执行轨迹原文
    pub params: String,           // ParamDef[] JSON
    pub output_schema: String,    // 输出格式描述
    pub input_schema: String,     // 输入参数定义 JSON
}

impl PlanFlexibleSnapshot {
    /// 是否有任何有效数据（首次执行返回 false）。
    pub fn has_data(&self) -> bool {
        !self.todos.is_empty() || !self.previous_summary.is_empty()
    }
}

/// 执行步骤（用于 ExecutionView 渲染）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExecutionStep {
    /// 步骤序号
    pub index: usize,
    /// 工具名称
    pub tool_name: String,
    /// 参数摘要
    pub params_summary: String,
    /// 结果摘要
    pub result_summary: String,
    /// 步骤状态
    pub status: StepStatus,
    /// 意外调整说明（仅 Warning 时有值）
    pub warning_detail: Option<String>,
    /// 耗时（毫秒）
    pub duration_ms: Option<u64>,
    /// 完整参数（工具调用完整 JSON，不截断；未完成时 None）
    pub params_data: Option<Value>,
    /// 完整输出（工具返回的原始内容，不截断；未完成时 None）
    pub result_data: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StepStatus {
    Pending,
    Running,
    Done,
    Warning,
    Failed,
}
