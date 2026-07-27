use async_trait::async_trait;
use anyhow::Result;

use super::react_types::*;
use crate::planner::coarse::CoarseGrainedStep;
use crate::types::PlanContext;

/// ReAct Agent trait
/// 通过行动-观察循环，智能执行任务。
/// think+act 合并为单次 LLM 调用（LLM 自主输出 tool_calls 或 DONE）。
#[async_trait]
pub trait ReActAgent: Send + Sync {
    /// 执行粗粒度步骤
    async fn execute_coarse_step(
        &mut self,
        coarse_step: &CoarseGrainedStep,
        context: &PlanContext,
    ) -> Result<ReActExecutionResult>;

    /// 执行：调用工具
    /// `current_intent` 与 `next_intent` 用于工具结果的后处理路由（如 HTML 清洗子 Agent），
    /// 不进入主 LLM 上下文，仅在工具结果后处理阶段使用。
    async fn execute_tool(
        &self,
        action: &Action,
        current_intent: &str,
        next_intent: &str,
    ) -> Result<Observation>;

    /// 观察：分析工具执行结果，判断是否完成目标。
    /// observe 可以访问并追加 messages 历史（&mut self），使结论回流到下轮迭代。
    async fn observe(
        &mut self,
        coarse_step: &CoarseGrainedStep,
        observation: &Observation,
    ) -> Result<ObserveResult>;

    /// 获取 Agent 名称
    fn name(&self) -> &str;
}
