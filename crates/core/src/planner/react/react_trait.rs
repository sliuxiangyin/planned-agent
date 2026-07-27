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
        &mut self,
        coarse_step: &CoarseGrainedStep,
        context: &PlanContext,
    ) -> Result<ReActExecutionResult>;
    
    /// 思考：分析当前状态，推理下一步
    async fn think(
        &mut self,
        coarse_step: &CoarseGrainedStep,
        remaining_steps: Option<&[CoarseGrainedStep]>,
    ) -> Result<Thought>;
    
    /// 行动：选择工具并生成参数
    async fn act(
        &mut self,
        coarse_step: &CoarseGrainedStep,
        thought: &Thought,
    ) -> Result<Action>;
    
    /// 执行：调用工具
    /// `current_intent` 与 `next_intent` 用于工具结果的后处理路由（如 HTML 清洗子 Agent），
    /// 不进入主 LLM 上下文，仅在工具结果后处理阶段使用。
    async fn execute_tool(
        &self,
        action: &Action,
        current_intent: &str,
        next_intent: &str,
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
