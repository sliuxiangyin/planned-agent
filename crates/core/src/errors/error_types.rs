use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 系统错误类型
#[derive(Debug, Error, Serialize, Deserialize)]
pub enum PlanSystemError {
    /// 计划生成失败
    #[error("计划生成失败: {0}")]
    PlanGenerationFailed(String),

    /// 计划验证失败
    #[error("计划验证失败: {0}")]
    PlanValidationFailed(String),

    /// 步骤执行失败
    #[error("步骤执行失败: {step_id} - {reason}")]
    StepExecutionFailed {
        step_id: String,
        reason: String,
    },

    /// 工具调用失败
    #[error("工具调用失败: {tool_name} - {reason}")]
    ToolCallFailed {
        tool_name: String,
        reason: String,
    },

    /// 重规划失败
    #[error("重规划失败: {0}")]
    ReplanningFailed(String),

    /// 超时
    #[error("超时: 超过 {timeout_ms}ms")]
    Timeout {
        timeout_ms: u64,
    },

    /// 超过最大迭代次数
    #[error("超过最大迭代次数: {max_iterations}")]
    MaxIterationsExceeded {
        max_iterations: u32,
    },

    /// 超过最大重规划次数
    #[error("超过最大重规划次数: {max_replans}")]
    MaxReplansExceeded {
        max_replans: u32,
    },

    /// 用户取消
    #[error("用户取消")]
    UserCancelled,

    /// 内部错误
    #[error("内部错误: {0}")]
    InternalError(String),

    /// 配置错误
    #[error("配置错误: {0}")]
    ConfigError(String),

    /// 依赖错误
    #[error("依赖错误: {0}")]
    DependencyError(String),
}

/// 错误恢复策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorRecoveryStrategy {
    /// 重试
    Retry {
        max_attempts: u32,
        delay_ms: u64,
    },
    /// 回退到替代工具
    Fallback {
        alternative_tool: String,
    },
    /// 跳过当前步骤
    Skip,
    /// 请求用户输入
    RequestUserInput,
    /// 终止执行
    Abort,
}

/// 错误上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorContext {
    /// 错误类型
    pub error_type: String,
    /// 错误消息
    pub message: String,
    /// 相关步骤 ID
    pub step_id: Option<String>,
    /// 相关工具名称
    pub tool_name: Option<String>,
    /// 恢复策略
    pub recovery_strategy: ErrorRecoveryStrategy,
}

impl PlanSystemError {
    /// 检查是否可以重试
    pub fn is_retryable(&self) -> bool {
        match self {
            PlanSystemError::ToolCallFailed { .. } => true,
            PlanSystemError::Timeout { .. } => true,
            PlanSystemError::InternalError(_) => true,
            _ => false,
        }
    }

    /// 获取建议的恢复策略
    pub fn suggested_recovery_strategy(&self) -> ErrorRecoveryStrategy {
        match self {
            PlanSystemError::ToolCallFailed { .. } => ErrorRecoveryStrategy::Retry {
                max_attempts: 3,
                delay_ms: 1000,
            },
            PlanSystemError::Timeout { .. } => ErrorRecoveryStrategy::Retry {
                max_attempts: 2,
                delay_ms: 2000,
            },
            PlanSystemError::MaxIterationsExceeded { .. } => ErrorRecoveryStrategy::Abort,
            PlanSystemError::MaxReplansExceeded { .. } => ErrorRecoveryStrategy::Abort,
            PlanSystemError::UserCancelled => ErrorRecoveryStrategy::Abort,
            _ => ErrorRecoveryStrategy::Abort,
        }
    }
}

impl ErrorContext {
    /// 创建新的错误上下文
    pub fn new(
        error_type: String,
        message: String,
        recovery_strategy: ErrorRecoveryStrategy,
    ) -> Self {
        Self {
            error_type,
            message,
            step_id: None,
            tool_name: None,
            recovery_strategy,
        }
    }

    /// 设置步骤 ID
    pub fn with_step_id(mut self, step_id: String) -> Self {
        self.step_id = Some(step_id);
        self
    }

    /// 设置工具名称
    pub fn with_tool_name(mut self, tool_name: String) -> Self {
        self.tool_name = Some(tool_name);
        self
    }
}
