//! 工具调用流式增量累积器。
//!
//! LLM 在流式响应中可能按 index 把同一个 tool call 的 id / name / arguments
//! 分多次给出，本结构负责把同一 index 的多次 `DeltaToolCall` 拼成完整的
//! `ToolCall`，并跟踪是否已下发过 `ToolCallStart` 事件（避免重复下发）。

/// 单个 tool call 的流式累积状态。
#[derive(Debug)]
pub(crate) struct ToolCallAccumulator {
    pub id: String,
    pub name: String,
    pub arguments: String,
    /// 是否已下发过 `ChatEvent::ToolCallStart`
    pub start_emitted: bool,
    /// 缓冲 start_emitted 之前的参数片段，start 后一次性 flush
    pub pending_deltas: Vec<String>,
}

impl ToolCallAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
            start_emitted: false,
            pending_deltas: Vec::new(),
        }
    }
}