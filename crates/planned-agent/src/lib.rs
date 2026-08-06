//! planned-agent library —— Plan-and-Execute 流水线
//!
//! 三层 API：
//! 1. 粗粒度计划生成（[`LlmCoarsePlanner`] / [`CoarsePlanner`]）
//! 2. ReAct 单步执行（[`DefaultReActAgent`] / [`ReActAgent`]）
//! 3. 全流水线编排（[`PlanAndExecuteAgent`]）+ 轨迹记录（[`TraceRecorder`]）
//!
//! 注：`config` / `cli` / `agent` 模块仅 binary 可见，未在此处暴露。

pub mod planner;

// ─── chat 模块 ───────────────────────────────────────────────────────────────
pub mod chat;
pub use chat::{ChatConfig, ChatEvent, ChatResponse, ChatService, PendingUIAction};

// ─── 从 core 透传：粗粒度相关类型 ──────────────────────────────
pub use planned_agent_core::planner::coarse::{
    CoarseGrainedPlan, CoarseGrainedStep, PlanComplexity, RiskLevel,
    DataRequirement, CoarsePlanValidationResult, CoarsePlanner,
};

// ─── 从 core 透传：ReAct 相关类型 ───────────────────────────────
pub use planned_agent_core::planner::react::{
    ReActAgent, ReActAgentConfig, ReActStep, Thought, Action,
    Observation, ObserveResult, ReActExecutionResult,
};

// ─── 从 core 透传：共享上下文 ─────────────────────────────────
pub use planned_agent_core::types::PlanContext;

// ─── 本 crate：粗粒度实现 ────────────────────────────────────
pub use crate::planner::coarse::LlmCoarsePlanner;

// ─── 本 crate：ReAct 实现 + 编排 ─────────────────────────────
pub use crate::planner::react::chunk::executor_context::ExecutorContext;
pub use crate::planner::react::{
    DefaultReActAgent, FlexiblePlanAgent, FlexiblePlanResult,
    PlanAndExecuteAgent, PlanAndExecuteConfig,
    PlanAndExecuteResult, StepResult, StepStore,
};

// ─── 本 crate：轨迹记录 ──────────────────────────────────────
pub use crate::planner::trace::recorder::TraceRecorderConfig;
pub use crate::planner::trace::TraceRecorder;