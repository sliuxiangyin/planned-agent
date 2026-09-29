//! 执行进度事件：执行器对外的唯一进度通道。
//!
//! 宿主（含 GUI）只需实现 [`PlanRunSink`]，把事件映射成自己的展示结构；
//! 执行器本身不感知任何 UI 概念。

use tokio::sync::mpsc::UnboundedSender;

use super::report::{PlanRunReport, StepRunRecord};

/// 执行过程中的进度事件。
#[derive(Debug, Clone)]
pub enum PlanRunEvent {
    /// 整次执行开始。
    RunStarted { total_steps: usize },
    /// 某一步开始（`intent` 已展开占位符）。
    StepStarted { index: usize, intent: String },
    /// 某一步某一轮的思考文本。
    ///
    /// 当前 LLM 走非流式调用，故这是"一轮一块"而非逐字增量；
    /// 若将来改流式，仅是同一事件的发送频率变高，对外契约不变。
    StepThought {
        index: usize,
        round: usize,
        text: String,
    },
    /// 某一步调用了工具。
    StepToolCall {
        index: usize,
        tool: String,
        /// 关键入参（**已渲染成一行**，供进度展示直接显示，如 `C:/Users/x/Downloads`）。
        ///
        /// 这里给渲染好的字符串而非原始 JSON：消费方（宿主 UI）不必再关心该挑哪几个字段，
        /// 快照也能直接把它当作轨迹的一行存下来。
        args: String,
        /// 工具是否执行成功（`!ToolResult.is_error`）。
        ok: bool,
    },
    /// 某一步结束（`record` 含该步的耗时 / token / 工具次数）。
    StepFinished {
        index: usize,
        record: StepRunRecord,
    },
    /// 整次执行结束。
    RunFinished { report: PlanRunReport },
    /// 执行失败（`index = None` 表示在执行器层面失败，如模板非法）。
    Failed {
        index: Option<usize>,
        error: String,
    },
}

/// 事件接收端：执行器每产生一个事件就调用一次 [`PlanRunSink::emit`]。
///
/// 实现必须是线程安全的，且**不应阻塞**（执行器在步骤循环内同步调用）。
pub trait PlanRunSink: Send + Sync {
    /// 接收一个事件。实现方不应 panic，也不应长时间阻塞。
    fn emit(&self, event: PlanRunEvent);
}

/// 基于 `mpsc::UnboundedSender` 的 sink。
///
/// 接收端被 drop 后事件会被静默丢弃 —— 进度展示不应拖垮执行本身。
pub struct ChannelSink {
    tx: UnboundedSender<PlanRunEvent>,
}

impl ChannelSink {
    /// 由发送端构造。
    pub fn new(tx: UnboundedSender<PlanRunEvent>) -> Self {
        Self { tx }
    }
}

impl PlanRunSink for ChannelSink {
    fn emit(&self, event: PlanRunEvent) {
        // 接收端已关闭（宿主不再关心进度）→ 丢弃，不影响执行
        let _ = self.tx.send(event);
    }
}

/// 什么都不做的 sink（测试与非交互场景用）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl PlanRunSink for NullSink {
    fn emit(&self, _event: PlanRunEvent) {}
}
