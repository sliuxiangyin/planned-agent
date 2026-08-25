//! 聊天流程数据结构。
//!
//! `PendingUI`、`ToolCallPhase`、`ToolCallEntry`、`ChatMessage` ——
//! 纯数据类型，不含业务逻辑。

use planned_agent_core::ai::types::Message;
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

/// 单次 Tool 调用的 UI 状态条目（轻量）。
///
/// 只存 UI 独有状态：phase / result / is_error；
/// `name` / `arguments` 的权威在 `Message.tool_calls`（渲染时由
/// [`ToolViewData`] 组合），不再在此双写，避免多副本同步错位。
#[derive(Clone, Debug)]
pub struct ToolCallEntry {
    /// 关联的 tool_call_id（`ToolCallStart` 的 id / `Message.tool_calls[].id`），
    /// 用于与 `Message.tool_calls` 一一关联。
    pub tool_call_id: String,
    /// 执行阶段
    pub phase: ToolCallPhase,
    /// 执行结果（ToolExecuted.content）
    pub result: Option<serde_json::Value>,
    /// 是否出错
    pub is_error: bool,
}

/// 手动实现 PartialEq：跳过 Value 字段（不支持 PartialEq），比较其余字段。
impl PartialEq for ToolCallEntry {
    fn eq(&self, other: &Self) -> bool {
        self.tool_call_id == other.tool_call_id
            && self.phase == other.phase
            && self.is_error == other.is_error
    }
}

/// ToolView 的渲染数据：`name`/`arguments`（来自 `Message.tool_calls`）
/// + UI 状态（`phase`/`result`/`is_error`，来自 [`ToolCallEntry`]）的组合。
#[derive(Clone, Debug)]
pub struct ToolViewData {
    /// tool_call_id（用于 Tool 消息回填 result 的关联键）
    pub tool_call_id: String,
    /// tool 名称
    pub name: String,
    /// 完整参数（JSON 字符串）
    pub arguments: String,
    /// 执行阶段
    pub phase: ToolCallPhase,
    /// 执行结果
    pub result: Option<serde_json::Value>,
    /// 是否出错
    pub is_error: bool,
}

/// 手动实现 PartialEq：跳过 Value 字段，比较其余字段。
impl PartialEq for ToolViewData {
    fn eq(&self, other: &Self) -> bool {
        self.tool_call_id == other.tool_call_id
            && self.name == other.name
            && self.arguments == other.arguments
            && self.phase == other.phase
            && self.is_error == other.is_error
    }
}

// ── 聊天消息 ──────────────────────────────────────────────────────────────

/// GUI 层的消息包装，自包含所有 UI 状态。
#[derive(Clone, Debug)]
pub struct ChatMessage {
    /// 底层消息数据（role、content、reasoning_content、tool_calls 等）
    pub message: Message,
    /// 显示序号（递增，用于前端稳定排序/动画 key）
    pub sequence_order: u64,
    /// 是否正在 streaming（替代 `streaming_idx` 游标）
    pub is_streaming: bool,
    /// Tool 消息的关联 ID（role=Tool 时用于匹配 Assistant 的 tool_call）
    pub tool_call_id: Option<String>,
    /// UI 层 Tool 调用状态（phase/is_error），持久化时不存储
    pub tool_call_entries: Vec<ToolCallEntry>,
}

