//! 灵活计划执行管理器：把一次计划执行做成**应用级后台任务**。
//!
//! 与「页面级状态」的三点区别：
//! - 任务用 `spawn_forever` 投到 root scope，**不随任何组件卸载而取消** ——
//!   切到别的页面、退回首页，它照跑；
//! - 状态存在本管理器自己的 `Signal` 里，组件挂载/卸载读的都是同一份，
//!   所以重新进入页面能直接看到进行中的进度，而不是重头开始；
//! - 唯一的主动中断途径是 [`FlexibleRunManager::stop`]（界面上的停止按钮），
//!   关软件才随进程终止。
//!
//! **本类型必须全局唯一**：两个实例就是两份运行状态，界面和后台会对不上。
//! 因此它由 `main.rs` 的 `ReadyShell` 在 app 级注入，消费方一律
//! `require_resource::<FlexibleRunManager>()`，**不要**就地 `new` 一个。
//!
//! 将来加定时执行：直接调 [`FlexibleRunManager::start`] 即可 ——
//! 它的入参只有「会话 + 模板 + 参数」，完全不碰界面状态。

use std::collections::HashMap;
use std::sync::Arc;

use dioxus::prelude::*;
// `spawn_forever` 不在 dioxus 的 prelude 里（prelude 只挑了 `spawn` 等一批名字），
// 但它是本模块的关键 —— 任务挂到 ROOT scope 才不会被组件卸载取消，故显式走 `dioxus::core`。
use dioxus::core::spawn_forever;
use planned_agent::flexible::{
    ChannelSink, ExecutorConfig, FlexibleExecutor, FlexiblePlanTemplate, PlanRunEvent,
    PlanRunParams, PlanRunReport, StepRunRecord, StepStatus,
};
use tokio::sync::{mpsc, watch};

use crate::context::{AiContext, ToolsContext};

/// 单步在界面上的相位。
///
/// `planned_agent` 的 `StepStatus` 只覆盖**终态**（Done / Failed / Skipped），
/// 装不下「还没轮到」和「正在跑」，故界面侧另立一套，由事件驱动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepPhase {
    /// 尚未开始（含等待前序步骤）
    Pending,
    /// 正在执行
    Running,
    Done,
    Failed,
    /// 未执行：被取消或前序失败
    Skipped,
}

impl StepPhase {
    /// 步骤行的 CSS 修饰后缀（`plan-pipeline__step--{...}`）。
    pub fn css_suffix(self) -> &'static str {
        match self {
            StepPhase::Pending => "pending",
            StepPhase::Running => "running",
            StepPhase::Done => "done",
            StepPhase::Failed => "failed",
            StepPhase::Skipped => "skipped",
        }
    }

    /// 节点图标的 CSS 修饰后缀（`plan-pipeline-node--{...}`）。
    pub fn node_suffix(self) -> &'static str {
        self.css_suffix()
    }

    /// 连接线的 CSS 修饰后缀。
    ///
    /// CSS 里运行态那根线叫 `--active`（而非 `--running`），故单独映射一次。
    pub fn line_suffix(self) -> &'static str {
        match self {
            StepPhase::Running => "active",
            other => other.css_suffix(),
        }
    }
}

/// 一次执行的总体状态。`Idle`（从未执行）表现为状态表里**没有**该会话的条目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    /// 跑完（含「有步骤失败」—— 单步失败不影响整体跑完，细节看 `report`）
    Finished,
    /// 整次执行异常终止（AI 不可用、执行器返回 Err）
    Failed,
}

/// 某个会话当前的执行状态。
#[derive(Debug, Clone, PartialEq)]
pub struct RunState {
    pub status: RunStatus,
    /// 与模板步骤 1:1 对齐的相位（PIPELINE 渲染用）。
    pub phases: Vec<StepPhase>,
    /// 与模板步骤 1:1 对齐的单步记录（跑过的步骤才有值）。
    ///
    /// 目前只写入、尚未被读取 —— 它是给「执行中的实时指标」（边跑边刷耗时 / token）
    /// 预留的：`StepFinished` 事件已带全部字段，不存下来将来就得改事件层。
    pub records: Vec<Option<StepRunRecord>>,
    /// 执行结束后才有（STATS 用）。
    pub report: Option<PlanRunReport>,
    /// 首个错误信息（单步失败或整次失败）。
    pub error: Option<String>,
}

impl RunState {
    fn new(total_steps: usize) -> Self {
        Self {
            status: RunStatus::Running,
            phases: vec![StepPhase::Pending; total_steps],
            records: vec![None; total_steps],
            report: None,
            error: None,
        }
    }

    /// 把一个执行事件并入状态。
    ///
    /// 注意 `PlanRunEvent::Failed` 的语义是**单步**失败（当前唯一来源是步骤 intent
    /// 展开失败，见 `executor.rs`），执行会继续跑后面的步骤 —— 所以它只改该步相位，
    /// 整次执行的成败由 `run()` 的返回值收尾。
    fn apply(&mut self, event: PlanRunEvent) {
        match event {
            PlanRunEvent::RunStarted { .. } => {}
            PlanRunEvent::StepStarted { index, .. } => {
                if let Some(phase) = self.phase_mut(index) {
                    *phase = StepPhase::Running;
                }
            }
            // THINK 终端尚未落地：事件有意不消费（产生它本身无害）。
            PlanRunEvent::StepThought { .. } => {}
            PlanRunEvent::StepToolCall { .. } => {}
            PlanRunEvent::StepFinished { index, record } => {
                if let Some(phase) = self.phase_mut(index) {
                    *phase = status_to_phase(record.status);
                }
                if let Some(slot) = self.record_mut(index) {
                    *slot = Some(record);
                }
            }
            PlanRunEvent::RunFinished { report } => {
                // 以报告整体对齐：没跑到的步骤（取消 / 前序失败）在这里补成 Skipped。
                for (offset, record) in report.steps.iter().enumerate() {
                    if let Some(phase) = self.phases.get_mut(offset) {
                        *phase = status_to_phase(record.status);
                    }
                    if let Some(slot) = self.records.get_mut(offset) {
                        *slot = Some(record.clone());
                    }
                }
                self.report = Some(report);
            }
            PlanRunEvent::Failed { index, error } => {
                if let Some(index) = index {
                    if let Some(phase) = self.phase_mut(index) {
                        *phase = StepPhase::Failed;
                    }
                }
                // 只留第一条：后续同类错误在各自 record.error 里，不必覆盖
                if self.error.is_none() {
                    self.error = Some(error);
                }
            }
        }
    }

    /// `index` 是 1-based（与事件一致），转成 0-based 下标。
    fn phase_mut(&mut self, index: usize) -> Option<&mut StepPhase> {
        index
            .checked_sub(1)
            .and_then(|offset| self.phases.get_mut(offset))
    }

    fn record_mut(&mut self, index: usize) -> Option<&mut Option<StepRunRecord>> {
        index
            .checked_sub(1)
            .and_then(|offset| self.records.get_mut(offset))
    }
}

fn status_to_phase(status: StepStatus) -> StepPhase {
    match status {
        StepStatus::Done => StepPhase::Done,
        StepStatus::Failed => StepPhase::Failed,
        StepStatus::Skipped => StepPhase::Skipped,
    }
}

/// 执行状态的两个信号（[`FlexibleRunManager`] 的存储）。
///
/// **必须在 `ScopeId::ROOT`（即 `main.rs` 的 `app()` 组件）创建**，不能放进 `ReadyShell`：
/// 后台任务用 `spawn_forever` 投到 ROOT scope，而 dioxus 要求「使用信号的 scope 必须是
/// owner 的子孙」。`ReadyShell` 位于 ROOT **之下**，方向正好相反，于是会触发
/// `dioxus_signals::warnings::__copy_value_hoisted`。
///
/// 且这不只是警告：`ReadyShell` 一旦被重建，信号就会先于后台任务被 drop，
/// 任务后续对它的写入会落空（状态凭空不动，界面永远停在“执行中”）。
///
/// 所以这里只承载「在哪创建」这件事：由 `app()` 创建后经 context 往下传。
#[derive(Clone, Copy)]
pub struct FlexibleRunSignals {
    pub states: Signal<HashMap<String, RunState>, SyncStorage>,
    pub cancels: Signal<HashMap<String, watch::Sender<bool>>, SyncStorage>,
}

/// 应用级执行管理器。**全局唯一**（见模块文档）。
pub struct FlexibleRunManager {
    ai: Arc<AiContext>,
    tools: Arc<ToolsContext>,
    /// 各会话的执行状态。组件读它即订阅；后台任务写它即刷新界面。
    states: Signal<HashMap<String, RunState>, SyncStorage>,
    /// 各会话的取消句柄（只有 `stop` 用）。
    cancels: Signal<HashMap<String, watch::Sender<bool>>, SyncStorage>,
}

impl FlexibleRunManager {
    /// 由 `ReadyShell` 构造（见模块文档：不要在其他地方 `new`）。
    ///
    /// `states` / `cancels` 来自 [`FlexibleRunSignals`]（在 ROOT scope 创建），
    /// 不可在这里现建 —— 否则会被 `spawn_forever` 使用后被判为跨 scope 悬挂。
    pub fn new(
        ai: Arc<AiContext>,
        tools: Arc<ToolsContext>,
        states: Signal<HashMap<String, RunState>, SyncStorage>,
        cancels: Signal<HashMap<String, watch::Sender<bool>>, SyncStorage>,
    ) -> Self {
        Self {
            ai,
            tools,
            states,
            cancels,
        }
    }

    /// 全部会话的执行状态。
    ///
    /// 渲染期要读它（读即订阅，状态一变界面就刷新）：拿 `session_id` 取自己那份。
    pub fn states(&self) -> Signal<HashMap<String, RunState>, SyncStorage> {
        self.states
    }

    /// 启动一次后台执行。
    ///
    /// 返回即「已受理」——任务在后台跑，与调用方（组件）的存活无关。
    /// 入参只有「会话 + 模板 + 参数」，不依赖任何界面状态，故定时器可直接复用。
    pub fn start(
        &self,
        session_id: &str,
        template: FlexiblePlanTemplate,
        params: PlanRunParams,
    ) -> Result<(), String> {
        if self.is_running(session_id) {
            return Err("该会话已有任务在执行".to_string());
        }
        let ai = self
            .ai
            .manager
            .default()
            .map_err(|e| format!("没有可用的 AI 提供方：{e}"))?;
        let tools = self.tools.registry.clone();

        let total_steps = template.steps.len();
        self.states
            .write_unchecked()
            .insert(session_id.to_string(), RunState::new(total_steps));

        let (tx, mut rx) = mpsc::unbounded_channel::<PlanRunEvent>();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.cancels
            .write_unchecked()
            .insert(session_id.to_string(), cancel_tx);

        // ① 事件消费：把进度并进状态 signal。
        // 注意 `write()` 的 guard 只在同步块里持有，绝不跨 `.await`，否则会死锁。
        let states = self.states;
        let consumer_key = session_id.to_string();
        spawn_forever(async move {
            while let Some(event) = rx.recv().await {
                let mut all = states.write_unchecked();
                if let Some(state) = all.get_mut(&consumer_key) {
                    state.apply(event);
                }
            }
        });

        // ② 执行器：不绑定任何组件，故切页面 / 退回首页都不中断。
        let states = self.states;
        let run_key = session_id.to_string();
        let executor = FlexibleExecutor::new(
            ai,
            tools,
            ExecutorConfig {
                // 执行器要真正干活（读写文件、执行命令），故给「全部业务工具」：
                // 按 select_tools_by_tokens 的语义，"all" = 排除 Utility / SubAgent 的全部启用工具。
                allowed_tools: Some(vec!["all".to_string()]),
                ..Default::default()
            },
        );
        spawn_forever(async move {
            let sink = ChannelSink::new(tx);
            let outcome = executor.run(&template, &params, &sink, Some(cancel_rx)).await;
            // 收尾：`run()` 的返回值是最权威的结论（事件流的 RunFinished 只补 report）
            let mut all = states.write_unchecked();
            if let Some(state) = all.get_mut(&run_key) {
                match outcome {
                    Ok(_) => state.status = RunStatus::Finished,
                    Err(e) => {
                        state.status = RunStatus::Failed;
                        state.error = Some(format!("{e:#}"));
                    }
                }
            }
        });

        Ok(())
    }

    /// 请求停止：执行器会在步骤边界（以及每一步的工具循环内）看到取消并收尾。
    ///
    /// 这是**唯一**的主动中断途径；不做「组件卸载即取消」——那会让切页面误伤正在跑的任务。
    pub fn stop(&self, session_id: &str) {
        if let Some(tx) = self.cancels.read().get(session_id) {
            let _ = tx.send(true);
        }
    }

    fn is_running(&self, session_id: &str) -> bool {
        // 用 peek 而非 read：read 会检查当前 runtime，而将来定时执行会从
        // 非 UI 上下文调 `start`，那里 read 会 panic（peek 不检查、也不订阅）。
        self.states
            .peek()
            .get(session_id)
            .is_some_and(|state| state.status == RunStatus::Running)
    }
}
