//! 聊天流程数据结构。
//!
//! `PendingUI`、`ToolCallPhase`、`ToolCallEntry`、`ChatMessage` ——
//! 纯数据类型，不含业务逻辑。

use std::sync::Arc;

use planned_agent_core::ai::types::Message;
use planned_agent_core::events::UIAction;

use super::storage::ChatStorage;

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

/// 单次 Tool 调用的 UI 状态条目。
#[derive(Clone, Debug)]
pub struct ToolCallEntry {
    /// tool 名称
    pub name: String,
    /// 执行阶段
    pub phase: ToolCallPhase,
    /// 累积的参数原文（JSON 字符串）
    pub arguments: String,
    /// 执行结果（ToolExecuted.content）
    pub result: Option<serde_json::Value>,
    /// 是否出错
    pub is_error: bool,
}

/// 手动实现 PartialEq：跳过 Value 字段（不支持 PartialEq），比较其余字段。
impl PartialEq for ToolCallEntry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.phase == other.phase
            && self.arguments == other.arguments
            && self.is_error == other.is_error
    }
}

// ── 聊天消息 ──────────────────────────────────────────────────────────────

/// GUI 层的消息包装，自包含所有 UI 状态。
///
/// 替代了原来的并行信号（`reasoning_texts`、`streaming_idx`、`tool_call_entries`），
/// 让每条消息携带自身的推理文本、streaming 状态和工具调用条目。
#[derive(Clone)]
pub struct ChatMessage {
    /// 底层消息数据（role、content、reasoning_content、tool_calls 等）
    pub message: Message,
    /// 显示序号（递增，用于前端稳定排序/动画 key）
    pub sequence_order: u64,
    /// 是否正在 streaming（替代 `streaming_idx` 游标）
    pub is_streaming: bool,
    /// 本条消息关联的 Tool 调用条目（替代全局 `tool_call_entries` flat map）
    pub tool_call_entries: Vec<ToolCallEntry>,
}

// ── 会话上下文 ────────────────────────────────────────────────────────────

/// 不可变的会话上下文 —— 持久化存储 + plan_id。
///
/// 初始化时构造一次，之后只读，不参与响应式更新。
/// 与 `ChatSignals` 分离，避免用 `Signal` 包裹非响应式数据。
#[derive(Clone)]
pub struct ChatContext {
    /// 持久化存储后端
    pub storage: Arc<dyn ChatStorage>,
    /// 当前会话的 plan_id
    pub plan_id: String,
}

/// 手动实现 PartialEq：`Arc<dyn ChatStorage>` 不实现 PartialEq，
/// 比较 plan_id + Arc 指针相等性（同一实例即相等）。
impl PartialEq for ChatContext {
    fn eq(&self, other: &Self) -> bool {
        self.plan_id == other.plan_id && Arc::ptr_eq(&self.storage, &other.storage)
    }
}
