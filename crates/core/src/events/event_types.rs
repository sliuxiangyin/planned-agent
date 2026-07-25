use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// 用户交互类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum UserInteractionType {
    /// 确认计划
    PlanConfirmation,
    /// 取消执行
    ExecutionCancelled,
    /// 提供输入
    InputProvided,
    /// 确认重规划
    ReplanConfirmation,
}

/// 系统事件类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemEvent {
    /// 计划生成事件
    PlanGenerated {
        plan_id: String,
        step_count: usize,
        timestamp: DateTime<Utc>,
    },

    /// 步骤执行事件
    StepExecuted {
        plan_id: String,
        step_id: String,
        success: bool,
        duration_ms: u64,
        timestamp: DateTime<Utc>,
    },

    /// 重规划事件
    Replanned {
        plan_id: String,
        reason: String,
        new_step_count: usize,
        timestamp: DateTime<Utc>,
    },

    /// 错误事件
    ErrorOccurred {
        plan_id: String,
        error_type: String,
        message: String,
        timestamp: DateTime<Utc>,
    },

    /// 用户交互事件
    UserInteraction {
        plan_id: String,
        interaction_type: UserInteractionType,
        timestamp: DateTime<Utc>,
    },

    /// 工具调用事件
    ToolCalled {
        plan_id: String,
        step_id: String,
        tool_name: String,
        timestamp: DateTime<Utc>,
    },

    /// 工具结果事件
    ToolResult {
        plan_id: String,
        step_id: String,
        tool_name: String,
        success: bool,
        timestamp: DateTime<Utc>,
    },

    /// 计划开始事件
    PlanStarted {
        plan_id: String,
        user_goal: String,
        timestamp: DateTime<Utc>,
    },

    /// 计划完成事件
    PlanCompleted {
        plan_id: String,
        success: bool,
        duration_ms: u64,
        timestamp: DateTime<Utc>,
    },

    /// 监督器事件
    SupervisorEvent {
        plan_id: String,
        event_type: String,
        details: String,
        timestamp: DateTime<Utc>,
    },
}

impl SystemEvent {
    /// 获取事件时间戳
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            SystemEvent::PlanGenerated { timestamp, .. } => *timestamp,
            SystemEvent::StepExecuted { timestamp, .. } => *timestamp,
            SystemEvent::Replanned { timestamp, .. } => *timestamp,
            SystemEvent::ErrorOccurred { timestamp, .. } => *timestamp,
            SystemEvent::UserInteraction { timestamp, .. } => *timestamp,
            SystemEvent::ToolCalled { timestamp, .. } => *timestamp,
            SystemEvent::ToolResult { timestamp, .. } => *timestamp,
            SystemEvent::PlanStarted { timestamp, .. } => *timestamp,
            SystemEvent::PlanCompleted { timestamp, .. } => *timestamp,
            SystemEvent::SupervisorEvent { timestamp, .. } => *timestamp,
        }
    }

    /// 获取计划 ID
    pub fn plan_id(&self) -> &str {
        match self {
            SystemEvent::PlanGenerated { plan_id, .. } => plan_id,
            SystemEvent::StepExecuted { plan_id, .. } => plan_id,
            SystemEvent::Replanned { plan_id, .. } => plan_id,
            SystemEvent::ErrorOccurred { plan_id, .. } => plan_id,
            SystemEvent::UserInteraction { plan_id, .. } => plan_id,
            SystemEvent::ToolCalled { plan_id, .. } => plan_id,
            SystemEvent::ToolResult { plan_id, .. } => plan_id,
            SystemEvent::PlanStarted { plan_id, .. } => plan_id,
            SystemEvent::PlanCompleted { plan_id, .. } => plan_id,
            SystemEvent::SupervisorEvent { plan_id, .. } => plan_id,
        }
    }

    /// 创建计划生成事件
    pub fn plan_generated(plan_id: String, step_count: usize) -> Self {
        Self::PlanGenerated {
            plan_id,
            step_count,
            timestamp: Utc::now(),
        }
    }

    /// 创建步骤执行事件
    pub fn step_executed(
        plan_id: String,
        step_id: String,
        success: bool,
        duration_ms: u64,
    ) -> Self {
        Self::StepExecuted {
            plan_id,
            step_id,
            success,
            duration_ms,
            timestamp: Utc::now(),
        }
    }

    /// 创建重规划事件
    pub fn replanned(plan_id: String, reason: String, new_step_count: usize) -> Self {
        Self::Replanned {
            plan_id,
            reason,
            new_step_count,
            timestamp: Utc::now(),
        }
    }

    /// 创建错误事件
    pub fn error_occurred(
        plan_id: String,
        error_type: String,
        message: String,
    ) -> Self {
        Self::ErrorOccurred {
            plan_id,
            error_type,
            message,
            timestamp: Utc::now(),
        }
    }

    /// 创建用户交互事件
    pub fn user_interaction(
        plan_id: String,
        interaction_type: UserInteractionType,
    ) -> Self {
        Self::UserInteraction {
            plan_id,
            interaction_type,
            timestamp: Utc::now(),
        }
    }

    /// 创建工具调用事件
    pub fn tool_called(
        plan_id: String,
        step_id: String,
        tool_name: String,
    ) -> Self {
        Self::ToolCalled {
            plan_id,
            step_id,
            tool_name,
            timestamp: Utc::now(),
        }
    }

    /// 创建工具结果事件
    pub fn tool_result(
        plan_id: String,
        step_id: String,
        tool_name: String,
        success: bool,
    ) -> Self {
        Self::ToolResult {
            plan_id,
            step_id,
            tool_name,
            success,
            timestamp: Utc::now(),
        }
    }

    /// 创建计划开始事件
    pub fn plan_started(plan_id: String, user_goal: String) -> Self {
        Self::PlanStarted {
            plan_id,
            user_goal,
            timestamp: Utc::now(),
        }
    }

    /// 创建计划完成事件
    pub fn plan_completed(
        plan_id: String,
        success: bool,
        duration_ms: u64,
    ) -> Self {
        Self::PlanCompleted {
            plan_id,
            success,
            duration_ms,
            timestamp: Utc::now(),
        }
    }

    /// 创建监督器事件
    pub fn supervisor_event(
        plan_id: String,
        event_type: String,
        details: String,
    ) -> Self {
        Self::SupervisorEvent {
            plan_id,
            event_type,
            details,
            timestamp: Utc::now(),
        }
    }
}
