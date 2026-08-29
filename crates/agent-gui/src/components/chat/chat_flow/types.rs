//! 聊天流程数据结构。
//!
//! `PendingUI`、`ToolCallPhase`、`ToolViewData`、`Bubble` ——
//! 纯数据类型，不含业务逻辑。

use planned_agent_core::events::UIAction;

// ── UI 交互 ──────────────────────────────────────────────────────────────

/// 待处理的 UI 交互（`request_user_action` 卡片状态）。
#[derive(Clone, PartialEq)]
pub struct PendingUI {
    /// 展示给用户的引导文本
    pub message: String,
    /// 用户可选的动作列表
    pub actions: Vec<UIAction>,
    /// 对应的 LLM tool_call_id（`confirm_user_action` 按此回填 tool 消息）
    pub tool_call_id: String,
    /// 子 agent 的 run_id（`UIActionRequest` 中的 `session_id` 字段）：
    /// 非空 = 子 agent 挂起，走 `resume_sub_agent` 路径；空 = 主 agent 交互。
    pub run_id: Option<String>,
}

// ── Tool 调用 ────────────────────────────────────────────────────────────

/// Tool 调用的执行阶段。
#[derive(Clone, Debug, PartialEq)]
pub enum ToolCallPhase {
    /// 参数流式构建中（收到 ToolCallStart，尚未 ToolCallComplete）
    Pending,
    /// 参数已就绪，正在执行中（收到 ToolCallComplete，尚未 ToolExecuted）
    Running,
    /// 执行完成（收到 ToolExecuted，is_error = false）
    Completed,
    /// 执行出错（收到 ToolExecuted，is_error = true）
    Error,
}

/// 单个 Tool 调用的渲染数据：`name`/`arguments` + UI 状态（`phase`/`result`/`is_error`）。
///
/// 单一数据源：事件直接驱动本结构（`tool_call_start` 建、`append_args`/`complete`/`executed`
/// 就地更新），不再有「`Message.tool_calls` + `tool_call_entries`」双源同步问题。
#[derive(Clone, Debug)]
pub struct ToolViewData {
    /// tool_call_id（`ToolCallStart` 的 id），用于精确路由后续事件
    pub tool_call_id: String,
    /// tool 名称
    pub name: String,
    /// 完整参数（JSON 字符串，流式累积 / `ToolCallComplete` 覆写为 pretty JSON）
    pub arguments: String,
    /// 执行阶段
    pub phase: ToolCallPhase,
    /// 执行结果（`ToolExecuted.content`）
    pub result: Option<serde_json::Value>,
    /// 是否出错
    pub is_error: bool,
}

/// 手动实现 PartialEq：跳过 Value 字段（不支持 PartialEq），比较其余字段。
impl PartialEq for ToolViewData {
    fn eq(&self, other: &Self) -> bool {
        self.tool_call_id == other.tool_call_id
            && self.name == other.name
            && self.arguments == other.arguments
            && self.phase == other.phase
            && self.is_error == other.is_error
    }
}

// ── 气泡 ─────────────────────────────────────────────────────────────────

/// 一条气泡（扁平化：消息数据 + 渲染数据合体，单一数据源）。
///
/// 不再区分 `Message` / `ChatMessage` / `RenderMessage` 三层：
/// 流式事件直接增量更新本结构；历史加载时由 [`crate::components::chat::chat_flow::signals::build_bubbles`]
/// 从服务端 `Message` 全量重建。
#[derive(Clone, Debug, PartialEq)]
pub struct Bubble {
    /// `false` = user 气泡，`true` = assistant 气泡
    pub is_assistant: bool,
    /// 可显示文本
    pub text: String,
    /// 思考链内容（assistant 独有，可为空）
    pub reasoning: String,
    /// 是否正在流式接收（驱动光标 / 脉冲动画）
    pub is_streaming: bool,
    /// 工具面板（仅 assistant 使用）
    pub tool_calls: Vec<ToolViewData>,
}
