use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// 计划上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanContext {
    /// 用户ID，用于多用户场景下的个性化计划生成和权限控制
    pub user_id: Option<String>,
    /// 会话ID，用于跟踪一次完整的对话/任务流程，支持会话级别的上下文保持
    pub session_id: Option<String>,
    /// 历史记录，存储之前的对话或操作历史，用于上下文理解
    pub history: Vec<String>,
    /// 扩展元数据，存储无法预定义的动态数据，支持灵活的业务扩展
    /// 常见字段：language, priority, timeout_ms, max_steps, user_role 等
    pub metadata: HashMap<String, Value>,
}

/// 计划
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub title: String,
    pub description: String,
    pub steps: Vec<PlanStep>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 计划步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub order: u32,
    pub action: String,
    pub parameters: Value,
    pub dependencies: Vec<String>,
    pub status: PlanStepStatus,
}

/// 计划步骤状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Skipped,
}

/// 计划执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecution {
    pub plan_id: String,
    pub status: PlanExecutionStatus,
    pub results: Vec<PlanStepResult>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 计划执行状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlanExecutionStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// 计划步骤结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStepResult {
    pub step_id: String,
    pub success: bool,
    pub output: Value,
    pub error: Option<String>,
    pub duration_ms: u64,
}
