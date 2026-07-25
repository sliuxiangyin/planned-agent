use async_trait::async_trait;
use anyhow::Result;

use super::react_types::*;
use crate::planner::coarse::CoarseGrainedStep;
use crate::types::PlanContext;

/// ReAct Agent trait
/// 通过思考-行动-观察循环，智能执行任务
#[async_trait]
pub trait ReActAgent: Send + Sync {
    /// 执行粗粒度步骤
    async fn execute_coarse_step(
        &self,
        coarse_step: &CoarseGrainedStep,
        context: &PlanContext,
    ) -> Result<ReActExecutionResult>;
    
    /// 思考：分析当前状态，推理下一步
    async fn think(
        &self,
        coarse_step: &CoarseGrainedStep,
        history: &[ReActStep],
        context: &PlanContext,
        remaining_steps: Option<&[CoarseGrainedStep]>,
    ) -> Result<Thought>;
    
    /// 行动：选择工具并生成参数
    async fn act(
        &self,
        thought: &Thought,
        context: &PlanContext,
    ) -> Result<Action>;
    
    /// 执行：调用工具
    async fn execute_tool(
        &self,
        action: &Action,
    ) -> Result<Observation>;
    
    /// 观察：分析工具执行结果，判断是否完成目标，提取关键信息
    async fn observe(
        &self,
        coarse_step: &CoarseGrainedStep,
        observation: &Observation,
    ) -> Result<ObserveResult>;
    
    /// 判断是否完成
    fn is_complete(&self, observation: &Observation) -> bool;
    
    /// 获取 Agent 名称
    fn name(&self) -> &str;
}
