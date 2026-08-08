use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::planner::coarse::{CoarseGrainedPlan, CoarseGrainedStep};
use crate::planner::types::PlanStepResult;

/// 重规划动作
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ReplanningAction {
    /// 继续执行原计划
    Continue,
    /// 更新计划
    UpdatePlan,
    /// 中止执行
    Abort,
    /// 请求用户输入
    RequestUserInput,
}

/// 重规划请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplanningRequest {
    /// 原始计划
    pub original_plan: CoarseGrainedPlan,
    /// 执行结果
    pub execution_results: Vec<PlanStepResult>,
    /// 剩余步骤
    pub remaining_steps: Vec<CoarseGrainedStep>,
    /// 用户目标
    pub user_goal: String,
    /// 当前上下文
    pub current_context: Value,
}

/// 重规划响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplanningResponse {
    /// 重规划动作
    pub action: ReplanningAction,
    /// 更新后的计划（仅当 action 为 UpdatePlan 时有值）
    pub updated_plan: Option<CoarseGrainedPlan>,
    /// 更新后的步骤（仅当 action 为 UpdatePlan 时有值）
    pub updated_steps: Option<Vec<CoarseGrainedStep>>,
    /// 重规划原因
    pub reason: String,
}

/// 重规划上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplanningContext {
    /// 用户目标
    pub user_goal: String,
    /// 当前执行状态
    pub execution_status: String,
    /// 失败的步骤
    pub failed_steps: Vec<String>,
    /// 成功的步骤
    pub successful_steps: Vec<String>,
    /// 当前上下文
    pub current_context: Value,
}

impl ReplanningRequest {
    /// 创建新的重规划请求
    pub fn new(
        original_plan: CoarseGrainedPlan,
        execution_results: Vec<PlanStepResult>,
        remaining_steps: Vec<CoarseGrainedStep>,
        user_goal: String,
        current_context: Value,
    ) -> Self {
        Self {
            original_plan,
            execution_results,
            remaining_steps,
            user_goal,
            current_context,
        }
    }

    /// 计算失败率
    pub fn failure_rate(&self) -> f32 {
        if self.execution_results.is_empty() {
            return 0.0;
        }
        let failed_count = self.execution_results.iter().filter(|r| !r.success).count() as f32;
        failed_count / self.execution_results.len() as f32
    }

    /// 获取失败的步骤
    pub fn failed_steps(&self) -> Vec<&PlanStepResult> {
        self.execution_results.iter().filter(|r| !r.success).collect()
    }

    /// 获取成功的步骤
    pub fn successful_steps(&self) -> Vec<&PlanStepResult> {
        self.execution_results.iter().filter(|r| r.success).collect()
    }
}

impl ReplanningResponse {
    /// 创建继续执行的响应
    pub fn continue_execution(reason: String) -> Self {
        Self {
            action: ReplanningAction::Continue,
            updated_plan: None,
            updated_steps: None,
            reason,
        }
    }

    /// 创建更新计划的响应
    pub fn update_plan(
        updated_plan: CoarseGrainedPlan,
        updated_steps: Vec<CoarseGrainedStep>,
        reason: String,
    ) -> Self {
        Self {
            action: ReplanningAction::UpdatePlan,
            updated_plan: Some(updated_plan),
            updated_steps: Some(updated_steps),
            reason,
        }
    }

    /// 创建中止执行的响应
    pub fn abort(reason: String) -> Self {
        Self {
            action: ReplanningAction::Abort,
            updated_plan: None,
            updated_steps: None,
            reason,
        }
    }

    /// 创建请求用户输入的响应
    pub fn request_user_input(reason: String) -> Self {
        Self {
            action: ReplanningAction::RequestUserInput,
            updated_plan: None,
            updated_steps: None,
            reason,
        }
    }

    /// 检查是否需要更新计划
    pub fn needs_plan_update(&self) -> bool {
        self.action == ReplanningAction::UpdatePlan
    }
}

impl ReplanningContext {
    /// 创建新的重规划上下文
    pub fn new(user_goal: String, current_context: Value) -> Self {
        Self {
            user_goal,
            execution_status: String::new(),
            failed_steps: Vec::new(),
            successful_steps: Vec::new(),
            current_context,
        }
    }

    /// 设置执行状态
    pub fn with_execution_status(mut self, status: String) -> Self {
        self.execution_status = status;
        self
    }

    /// 添加失败步骤
    pub fn add_failed_step(&mut self, step_id: String) {
        self.failed_steps.push(step_id);
    }

    /// 添加成功步骤
    pub fn add_successful_step(&mut self, step_id: String) {
        self.successful_steps.push(step_id);
    }
}
