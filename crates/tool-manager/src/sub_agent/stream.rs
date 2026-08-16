//! 工具执行过程流协议。
//!
//! 子 agent 工具在执行过程中通过 [`ToolStreamSender`] 发射过程事件（状态/文本增量/
//! 内部工具调用/最终摘要），经 `tokio::sync::mpsc` 旁路通道实时送达调用方
//! （如 `chat_with_callback` 主循环）。
//!
//! 设计约束：
//! - 事件只带工具层概念（`tool_name` + `invocation_id` + `seq`），不携带
//!   planner 层的 `plan_id`/`step_id`，上层自行映射；
//! - 过程流**不进主 agent 的 LLM 上下文**（业界共识：上下文只收最终结果），
//!   最终结果仍由 `ToolResult` 经既有闭环返回。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use planned_agent_core::events::ChatEvent;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;

/// 过程流事件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StreamKind {
    /// 生命周期状态：started / running / finished / failed
    Status,
    /// 文本增量（打字机效果）
    TextDelta,
    /// 子 agent 内部工具调用（`data` 为 name + args 的 JSON 字符串）
    ToolCall,
    /// 子 agent 最终结论摘要（简短版；完整结果走 `ToolResult`）
    FinalSummary,
}

/// 工具执行过程流事件
#[derive(Debug, Clone, Serialize)]
pub struct ToolStreamEvent {
    /// 被调用的工具名称（注册名）
    pub tool_name: String,
    /// 调用实例 ID，与最终 `ToolResult.call_id` 一致
    pub invocation_id: String,
    /// 同 invocation 内单调递增序号（保序）
    pub seq: u64,
    pub kind: StreamKind,
    /// 负载：文本增量 / 状态描述 / 内部工具调用 JSON
    pub data: String,
    /// 结构化聊天事件（子 agent 旁路类型化直传）：`Some` 时优先于
    /// `kind`/`data`，`kind` 固定为兼容值；旧 runner 发射的字符串事件为 `None`。
    #[serde(default)]
    pub event: Option<ChatEvent>,
    pub timestamp: DateTime<Utc>,
}

/// 发射句柄：子 agent 执行器持有，向调用方推送过程事件。
///
/// 内部包装 `mpsc::Sender`，`emit` 为阻塞式发送（背压传导、不丢事件）。
/// 通过 [`ToolStreamSender::disabled`] 构造的句柄所有 `emit` 均为 no-op
/// （无人接收时 `send` 立即返回 `Err`，被吞掉），用于非流式调用场景。
#[derive(Clone)]
pub struct ToolStreamSender {
    tx: mpsc::Sender<ToolStreamEvent>,
    tool_name: String,
    invocation_id: String,
    seq: Arc<AtomicU64>,
}

impl ToolStreamSender {
    /// 创建新的发射句柄（绑定工具名与调用实例 ID）
    pub fn new(
        tx: mpsc::Sender<ToolStreamEvent>,
        tool_name: impl Into<String>,
        invocation_id: impl Into<String>,
    ) -> Self {
        Self {
            tx,
            tool_name: tool_name.into(),
            invocation_id: invocation_id.into(),
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 创建 no-op 句柄：所有 `emit` 立即返回 `Ok`，不产生任何事件。
    ///
    /// 用于子 agent 被非流式入口（`ToolRegistry::call_tool`）调用时，
    /// runner 无需为"有无流"编写分支。
    pub fn disabled() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        Self {
            tx,
            tool_name: String::new(),
            invocation_id: String::new(),
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 本次调用的实例 ID（= 父 agent 调用子 agent 时的 `tool_call_id`，即 run_id）。
    pub fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    /// 被调用的工具名称（注册名）。
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    /// 发射一条事件（阻塞式发送；无接收者时为 no-op）
    pub async fn emit(&self, kind: StreamKind, data: impl Into<String>) -> Result<()> {
        let event = ToolStreamEvent {
            tool_name: self.tool_name.clone(),
            invocation_id: self.invocation_id.clone(),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            kind,
            data: data.into(),
            event: None,
            timestamp: Utc::now(),
        };
        // 无接收者时 send 返回 Err（如 disabled 句柄），此时静默丢弃
        let _ = self.tx.send(event).await;
        Ok(())
    }

    /// 发射结构化聊天事件（类型化直传，不经过字符串降级）。
    ///
    /// 子 agent 内部 `ChatService` 产生的事件经此原样送达调用方旁路；
    /// `kind` 置为兼容占位值（`TextDelta`），消费方以 `event` 为准。
    pub async fn emit_event(&self, ev: ChatEvent) -> Result<()> {
        let event = ToolStreamEvent {
            tool_name: self.tool_name.clone(),
            invocation_id: self.invocation_id.clone(),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            kind: StreamKind::TextDelta,
            data: String::new(),
            event: Some(ev),
            timestamp: Utc::now(),
        };
        // 无接收者时 send 返回 Err（如 disabled 句柄），此时静默丢弃
        let _ = self.tx.send(event).await;
        Ok(())
    }

    /// 发射生命周期状态
    pub async fn status(&self, s: impl Into<String>) -> Result<()> {
        self.emit(StreamKind::Status, s).await
    }

    /// 发射文本增量
    pub async fn text(&self, delta: impl Into<String>) -> Result<()> {
        self.emit(StreamKind::TextDelta, delta).await
    }

    /// 发射子 agent 内部工具调用（name + args 序列化为 JSON 字符串）
    pub async fn tool_call(&self, name: &str, args: &Value) -> Result<()> {
        let data = serde_json::json!({ "name": name, "arguments": args }).to_string();
        self.emit(StreamKind::ToolCall, data).await
    }

    /// 发射最终结论摘要
    pub async fn summary(&self, s: impl Into<String>) -> Result<()> {
        self.emit(StreamKind::FinalSummary, s).await
    }

    // ── 同步发射（try_send，通道满时丢弃）──
    // 用于同步回调（如 `ChatEvent` 的 `FnMut` 回调）内实时转发过程流；
    // 正常运行时主循环持续 recv，通道不会真实积压。

    /// 同步发射一条事件（通道满时静默丢弃）
    pub fn emit_sync(&self, kind: StreamKind, data: impl Into<String>) {
        let event = ToolStreamEvent {
            tool_name: self.tool_name.clone(),
            invocation_id: self.invocation_id.clone(),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            kind,
            data: data.into(),
            event: None,
            timestamp: Utc::now(),
        };
        let _ = self.tx.try_send(event);
    }

    /// 同步发射结构化聊天事件（类型化直传，供同步 `FnMut` 回调内调用）。
    pub fn emit_event_sync(&self, ev: ChatEvent) {
        let event = ToolStreamEvent {
            tool_name: self.tool_name.clone(),
            invocation_id: self.invocation_id.clone(),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            kind: StreamKind::TextDelta,
            data: String::new(),
            event: Some(ev),
            timestamp: Utc::now(),
        };
        let _ = self.tx.try_send(event);
    }

    /// 同步发射生命周期状态
    pub fn status_sync(&self, s: impl Into<String>) {
        self.emit_sync(StreamKind::Status, s);
    }

    /// 同步发射文本增量
    pub fn text_sync(&self, delta: impl Into<String>) {
        self.emit_sync(StreamKind::TextDelta, delta);
    }

    /// 同步发射工具调用（name + args 序列化为 JSON 字符串）
    pub fn tool_call_sync(&self, name: &str, args: &Value) {
        let data = serde_json::json!({ "name": name, "arguments": args }).to_string();
        self.emit_sync(StreamKind::ToolCall, data);
    }

    /// 同步发射最终结论摘要
    pub fn summary_sync(&self, s: impl Into<String>) {
        self.emit_sync(StreamKind::FinalSummary, s);
    }
}
