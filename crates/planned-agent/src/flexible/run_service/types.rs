//! 执行服务的对外数据形态：命令 / 快照 / 订阅。
//!
//! 全部为纯数据（不依赖 dioxus、不做 IO）：宿主直接消费。
//! 「事件 → 快照」的归并见 [`super::state`]，服务循环见 [`super::core`]。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use planned_agent_core::ai::AiClient;

use crate::flexible::event::PlanRunEvent;
use crate::flexible::executor::ExecutorConfig;
use crate::flexible::params::PlanRunParams;
use crate::flexible::report::{PlanRunReport, StepRunRecord, StepStatus};
use crate::flexible::template::{FlexiblePlanTemplate, PlanStep};

/// 会话 id（= `plans_flexible_sessions.id`）：服务里一切按它定位。
pub type SessionId = String;

/// 订阅句柄：`subscribe` 返回，`unsubscribe` 用它注销。
pub type SubscriptionId = u64;

/// 当前 UNIX 毫秒；系统时钟异常时退化为 `0`（不 panic）。
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// 一次执行的运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// 正在执行
    Running,
    /// 全部步骤成功
    Succeeded,
    /// 失败（单步失败或整次失败；「根本没启动起来」的失败由宿主 UI 呈现，不进快照）
    Failed,
    /// 用户取消
    Cancelled,
}

impl RunStatus {
    pub fn is_running(self) -> bool {
        matches!(self, RunStatus::Running)
    }

    /// 是否已到终态（终态快照必带 `finished_at_ms`）。
    pub fn is_finished(self) -> bool {
        !self.is_running()
    }
}

/// 单个步骤的相位（界面按它上色）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepPhase {
    /// 未执行
    Pending,
    /// 正在执行
    Running,
    /// 已完成
    Done,
    /// 执行失败
    Failed,
    /// 跳过（前序失败或用户取消）
    Skipped,
}

impl StepPhase {
    /// 由执行器的步骤终态转换。
    pub fn from_status(status: StepStatus) -> Self {
        match status {
            StepStatus::Done => StepPhase::Done,
            StepStatus::Failed => StepPhase::Failed,
            StepStatus::Skipped => StepPhase::Skipped,
        }
    }

    /// 步骤容器的 CSS 修饰名（`plan-pipeline__step--{suffix}`）。
    pub fn css_suffix(self) -> &'static str {
        match self {
            StepPhase::Pending => "pending",
            StepPhase::Running => "running",
            StepPhase::Done => "done",
            StepPhase::Failed => "failed",
            StepPhase::Skipped => "skipped",
        }
    }

    /// 节点圆点的 CSS 修饰名（`plan-pipeline-node--{suffix}`）。
    pub fn node_suffix(self) -> &'static str {
        self.css_suffix()
    }

    /// 连接线的 CSS 修饰名（`plan-pipeline__step-line--{suffix}`）。
    ///
    /// 与其他两个不同：激活态叫 `active`；`Skipped` 无独立样式，沿用 `pending`。
    pub fn line_suffix(self) -> &'static str {
        match self {
            StepPhase::Running => "active",
            StepPhase::Done => "done",
            StepPhase::Failed => "failed",
            StepPhase::Pending | StepPhase::Skipped => "pending",
        }
    }
}

/// 单个步骤的快照。
#[derive(Debug, Clone, PartialEq)]
pub struct StepSnapshot {
    /// 步骤序号（从 1 开始，与事件的 `index` 一致）。
    pub index: usize,
    /// 结果引用标识，如 `#E1`。
    pub result_reference: String,
    /// 子目标描述：先给模板原文，执行到该步时被**展开版**覆盖。
    pub intent: String,
    /// 可验证产出（模板原文，不展开）。
    pub expected_output: String,
    pub phase: StepPhase,
    pub duration_ms: u64,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub tool_calls: usize,
    pub error: Option<String>,
}

impl StepSnapshot {
    /// 模板骨架：`Pending` + 模板原文 `intent`。
    pub fn pending(index: usize, step: &PlanStep) -> Self {
        Self {
            index,
            result_reference: step.result_reference.clone(),
            intent: step.intent.clone(),
            expected_output: step.expected_output.clone(),
            phase: StepPhase::Pending,
            duration_ms: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            tool_calls: 0,
            error: None,
        }
    }

    /// 由执行记录生成（`StepFinished` 与 `RunFinished.report` 用）。
    pub fn from_record(record: &StepRunRecord) -> Self {
        Self {
            index: record.index,
            result_reference: record.result_reference.clone(),
            intent: record.intent.clone(),
            expected_output: record.expected_output.clone(),
            phase: StepPhase::from_status(record.status),
            duration_ms: record.duration_ms,
            prompt_tokens: record.prompt_tokens,
            completion_tokens: record.completion_tokens,
            tool_calls: record.tool_calls,
            error: record.error.clone(),
        }
    }

    /// 占位步骤：事件给出的 `index` 超出已知模板时的兜底。
    fn placeholder(index: usize) -> Self {
        Self {
            index,
            result_reference: format!("#E{index}"),
            intent: String::new(),
            expected_output: String::new(),
            phase: StepPhase::Pending,
            duration_ms: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            tool_calls: 0,
            error: None,
        }
    }

    pub fn total_tokens(&self) -> u32 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// 某会话一次执行的快照 —— 服务的核心状态（进度 + 状态）。
#[derive(Debug, Clone, PartialEq)]
pub struct RunSnapshot {
    pub session_id: SessionId,
    /// 本次执行的序号（每次 `start` 递增）：事件带旧 `run_id` 一律丢弃。
    pub run_id: u64,
    pub status: RunStatus,
    pub total_steps: usize,
    /// 当前执行到的步骤序号；未开始为 `None`。
    pub current_step: Option<usize>,
    pub steps: Vec<StepSnapshot>,
    /// 最近一条思考文本（THINK 展示用）。
    pub last_thought: Option<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    /// 整次失败的原因（单步失败的原因在 `steps[].error`）。
    pub error: Option<String>,
    /// 终态时带上的执行报告（STATS 块用）。
    pub report: Option<PlanRunReport>,
}

impl RunSnapshot {
    /// 起跑快照：按模板铺满 `Pending` 步骤，状态 `Running`。
    pub fn started(
        session_id: impl Into<SessionId>,
        run_id: u64,
        template: &FlexiblePlanTemplate,
    ) -> Self {
        let steps = template
            .steps
            .iter()
            .enumerate()
            .map(|(offset, step)| StepSnapshot::pending(offset + 1, step))
            .collect::<Vec<_>>();
        Self {
            session_id: session_id.into(),
            run_id,
            status: RunStatus::Running,
            total_steps: steps.len(),
            current_step: None,
            steps,
            last_thought: None,
            started_at_ms: now_ms(),
            finished_at_ms: None,
            error: None,
            report: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.status.is_running()
    }

    /// 已推进的步数（`Done` + `Failed`）—— 进度条的分子。
    pub fn progressed(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| matches!(step.phase, StepPhase::Done | StepPhase::Failed))
            .count()
    }

    /// 第 `offset`（0-based）步的相位；未知一律 `Pending`。
    pub fn phase_of(&self, offset: usize) -> StepPhase {
        self.steps
            .get(offset)
            .map(|step| step.phase)
            .unwrap_or(StepPhase::Pending)
    }

    /// 取第 `index`（1-based）步；缺失则先补齐前序步骤，再返回其可变引用。
    ///
    /// 补齐是必须的：事件里的 `index` 与 `total_steps` 共同决定 `N/M` 的展示，
    /// 少补中间步骤会让总数偏小（例如只到过第 2 步时总数记成 1）。
    pub(crate) fn step_or_insert(&mut self, index: usize) -> &mut StepSnapshot {
        while self.steps.len() < index {
            let next = self.steps.len() + 1;
            self.steps.push(StepSnapshot::placeholder(next));
        }
        if let Some(position) = self.steps.iter().position(|step| step.index == index) {
            self.total_steps = self.total_steps.max(self.steps.len());
            return &mut self.steps[position];
        }
        // 兜底：steps 里已有更大的 index（事件乱序）→ 按序插入
        let position = self.steps.partition_point(|step| step.index < index);
        self.steps.insert(position, StepSnapshot::placeholder(index));
        self.total_steps = self.total_steps.max(self.steps.len());
        &mut self.steps[position]
    }

    /// 补齐到 `total` 步（`RunStarted` 给出的总数可能大于已知骨架）。
    pub(crate) fn ensure_steps(&mut self, total: usize) {
        while self.steps.len() < total {
            let next = self.steps.len() + 1;
            self.steps.push(StepSnapshot::placeholder(next));
        }
    }
}

/// 订阅推送载荷：会话 id + 该会话的最新快照。
#[derive(Debug, Clone, PartialEq)]
pub struct RunUpdate {
    pub session_id: SessionId,
    pub snapshot: RunSnapshot,
}

/// 订阅的会话范围。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionFilter {
    /// 全部会话（总览用）
    All,
    /// 单个会话
    One(SessionId),
}

impl SessionFilter {
    /// 该会话的更新是否应推给这个订阅者。
    pub fn matches(&self, session_id: &str) -> bool {
        match self {
            SessionFilter::All => true,
            SessionFilter::One(id) => id == session_id,
        }
    }
}

/// 一次执行的完整输入：**能执行所需的一切都由调用方给**。
///
/// 内核不再定义「模板从哪来 / AI 从哪取」这类接缝（见设计稿 §12）：会话是否已定稿、
/// 模板能不能反序列化、有没有配 provider，都是宿主的问题；宿主解析完再把这四样递进来。
#[derive(Clone)]
pub struct RunRequest {
    pub session_id: SessionId,
    /// 已定稿且可解析的模板。
    pub template: FlexiblePlanTemplate,
    /// 渲染参数（默认值由调用方决定：`PlanRunParams::from_template`）。
    pub params: PlanRunParams,
    pub client: Arc<dyn AiClient>,
    /// 执行配置（与 `client` 同侧：都是 `FlexibleExecutor::new` 的入参，见 §12.1）。
    pub config: ExecutorConfig,
}

impl std::fmt::Debug for RunRequest {
    /// 手写而非 derive：`Arc<dyn AiClient>` 不实现 `Debug`。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunRequest")
            .field("session_id", &self.session_id)
            .field("steps", &self.template.steps.len())
            .field("provider", &self.client.provider_name())
            .field("model", &self.client.model_name())
            .finish_non_exhaustive()
    }
}

/// 送往服务循环的命令。
#[derive(Debug, Clone)]
pub enum RunCommand {
    /// 启动一次执行（输入自包含）。
    Start { request: RunRequest },
    /// 执行器事件的回送（由内部 sink 发出，宿主不用）。
    Event {
        session_id: SessionId,
        run_id: u64,
        event: PlanRunEvent,
    },
}
