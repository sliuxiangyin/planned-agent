use async_trait::async_trait;
use anyhow::Result;
use super::coarse_types::{CoarseGrainedPlan, CoarsePlanValidationResult};
use crate::planner::types::PlanContext;

/// 粗粒度计划生成器 trait
///
/// 负责分析用户需求，生成粗粒度计划（只定义"做什么"，不定义"怎么做"）
#[async_trait]
pub trait CoarsePlanner: Send + Sync {
    /// 从用户输入生成粗粒度计划
    ///
    /// # 参数
    /// - `input`: 用户输入的需求描述
    /// - `context`: 计划上下文信息
    ///
    /// # 返回
    /// 生成的粗粒度计划
    async fn generate_coarse_plan(
        &self,
        input: &str,
        context: &PlanContext,
    ) -> Result<CoarseGrainedPlan>;

    /// 验证粗粒度计划的合法性
    ///
    /// # 参数
    /// - `plan`: 待验证的粗粒度计划
    ///
    /// # 返回
    /// 验证结果，包含错误和警告信息
    async fn validate_coarse_plan(
        &self,
        plan: &CoarseGrainedPlan,
    ) -> Result<CoarsePlanValidationResult>;

    /// 获取计划生成器名称
    fn name(&self) -> &str;
}
