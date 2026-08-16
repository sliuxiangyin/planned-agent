//! planned-agent library —�?Plan-and-Execute 流水�?
//!
//! 三层 API�?
//! 1. 粗粒度计划生成（[`LlmCoarsePlanner`] / [`CoarsePlanner`]�?
//! 2. ReAct 单步执行（[`DefaultReActAgent`] / [`ReActAgent`]�?
//! 3. 全流水线编排（[`PlanAndExecuteAgent`]�? 轨迹记录（[`TraceRecorder`]�?
//!
//! 注：`config` / `cli` / `agent` 模块�?binary 可见，未在此处暴露�?

pub mod planner;

// ─── v2-chat 模块（内部维�?history + 后台 loop）───────────────
pub mod chat;
pub use chat::{
    ChatConfig, ChatEvent, ChatService, SendTicket, SubscriptionGuard, SubscriptionId,
};
pub use planned_agent_core::events::{
    FALLBACK_CONFIRM_ID, FALLBACK_CONFIRM_LABEL, MultiSelectOption, UIAction, UIActionType,
};

// ─── �?core 透传：粗粒度相关类型 ──────────────────────────────
pub use planned_agent_core::planner::coarse::{
    CoarseGrainedPlan, CoarseGrainedStep, PlanComplexity, RiskLevel,
    DataRequirement, CoarsePlanValidationResult, CoarsePlanner,
};

// ─── �?core 透传：ReAct 相关类型 ───────────────────────────────
pub use planned_agent_core::planner::react::{
    ReActAgent, ReActAgentConfig, ReActStep, Thought, Action,
    Observation, ObserveResult, ReActExecutionResult,
};

// ─── �?core 透传：共享上下文 ─────────────────────────────────
pub use planned_agent_core::planner::types::PlanContext;

// ─── �?crate：粗粒度实现 ────────────────────────────────────
pub use crate::planner::coarse::LlmCoarsePlanner;

// ─── �?crate：ReAct 实现 + 编排 ─────────────────────────────
pub use crate::planner::react::chunk::executor_context::ExecutorContext;
pub use crate::planner::react::{
    DefaultReActAgent,
    PlanAndExecuteAgent, PlanAndExecuteConfig,
    PlanAndExecuteResult, StepResult, StepStore,
};


// ─── �?crate：轨迹记�?──────────────────────────────────────
pub use crate::planner::trace::recorder::TraceRecorderConfig;
pub use crate::planner::trace::TraceRecorder;