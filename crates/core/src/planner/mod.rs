//! Planner 模块
//!
//! 包含 Coarse Planner、ReAct Agent、RePlanner 等规划器实现，
//! 以及 Plan 领域模型（见 [`types`]）。

pub mod coarse;
pub mod react;
pub mod replanner;
pub mod types;
pub mod validation;
pub mod trace;

// 注意：子模块内部类型不导出到 core 顶层
// 如需使用，通过完整路径访问：
//   planned_agent_core::planner::coarse::CoarsePlan
