//! 聊天事件（协议类型在 core 层）+ 子 agent 旁路事件
//!
//! `ChatEvent` 定义在 [`planned_agent_core::events`]（供 tool-manager 类型化
//! 承载），此处 re-export 保持 `planned_agent::chat::ChatEvent` 路径不变。
//! [`SubAgentChatEvent`]（原 `SubAgentOutput`）是子 agent 过程流旁路事件：
//! 携带子 agent 内部 `ChatService` 的**原始** `ChatEvent`，不再降级为字符串。

pub use planned_agent_core::events::ChatEvent;

/// 子 agent 过程流事件（旁路通道，供上层单独渲染子 agent 过程）。
///
/// 与主通道 `on_event` 分离：主 agent 事件走 `on_event`，子 agent 过程流走
/// `on_agent_event`。`event` 为子 agent 内部 `ChatService` 的原始 `ChatEvent`
/// （全量、无降级，含 `RoundStart`/`RoundEnd`/`ToolCallArgsDelta` 等此前被
/// 丢弃的事件）；子 agent 的**最终结果**仍以 `ChatEvent::ToolExecuted`
/// 出现在主通道中，二者通过 `invocation_id` 关联。
#[derive(Debug, Clone)]
pub struct SubAgentChatEvent {
    /// 注册的子 agent 名称 = 工具注册名（`tool.name`），与 LLM 调用名一致
    pub agent_name: String,
    /// 对应 LLM 的 tool_call_id；可关联主通道的 `ChatEvent::ToolExecuted`
    pub invocation_id: String,
    /// 同 invocation 内单调递增，保序
    pub seq: u64,
    /// 子 agent 内部 ChatService 的原始事件（结构化、无降级）
    pub event: ChatEvent,
}
