use serde::{Deserialize, Serialize};

use crate::planner::coarse::RiskLevel;

/// 验证错误类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ValidationErrorType {
    /// 循环依赖
    CircularDependency,
    /// 缺失依赖
    MissingDependency,
    /// 无效步骤
    InvalidStep,
    /// 工具未找到
    ToolNotFound,
    /// 风险过高
    RiskTooHigh,
}

/// 验证警告类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ValidationWarningType {
    /// 高风险
    HighRisk,
    /// 复杂依赖
    ComplexDependency,
    /// 长执行时间
    LongExecutionTime,
    /// 步骤过多
    ManySteps,
}

/// 验证错误
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationError {
    /// 错误类型
    pub error_type: ValidationErrorType,
    /// 错误消息
    pub message: String,
    /// 相关步骤 ID
    pub step_id: Option<String>,
}

/// 验证警告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationWarning {
    /// 警告类型
    pub warning_type: ValidationWarningType,
    /// 警告消息
    pub message: String,
    /// 相关步骤 ID
    pub step_id: Option<String>,
}

/// 依赖检查结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyCheckResult {
    /// 是否有效
    pub valid: bool,
    /// 循环依赖
    pub circular_dependencies: Vec<Vec<String>>,
    /// 缺失的依赖
    pub missing_dependencies: Vec<String>,
}

/// 计划验证结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanValidationResult {
    /// 是否有效
    pub valid: bool,
    /// 错误列表
    pub errors: Vec<ValidationError>,
    /// 警告列表
    pub warnings: Vec<ValidationWarning>,
    /// 风险等级
    pub risk_level: RiskLevel,
}

impl ValidationError {
    /// 创建新的验证错误
    pub fn new(error_type: ValidationErrorType, message: String) -> Self {
        Self {
            error_type,
            message,
            step_id: None,
        }
    }

    /// 创建带步骤 ID 的验证错误
    pub fn with_step_id(mut self, step_id: String) -> Self {
        self.step_id = Some(step_id);
        self
    }

    /// 创建循环依赖错误
    pub fn circular_dependency(message: String) -> Self {
        Self::new(ValidationErrorType::CircularDependency, message)
    }

    /// 创建缺失依赖错误
    pub fn missing_dependency(message: String) -> Self {
        Self::new(ValidationErrorType::MissingDependency, message)
    }

    /// 创建无效步骤错误
    pub fn invalid_step(message: String) -> Self {
        Self::new(ValidationErrorType::InvalidStep, message)
    }

    /// 创建工具未找到错误
    pub fn tool_not_found(message: String) -> Self {
        Self::new(ValidationErrorType::ToolNotFound, message)
    }

    /// 创建风险过高错误
    pub fn risk_too_high(message: String) -> Self {
        Self::new(ValidationErrorType::RiskTooHigh, message)
    }
}

impl ValidationWarning {
    /// 创建新的验证警告
    pub fn new(warning_type: ValidationWarningType, message: String) -> Self {
        Self {
            warning_type,
            message,
            step_id: None,
        }
    }

    /// 创建带步骤 ID 的验证警告
    pub fn with_step_id(mut self, step_id: String) -> Self {
        self.step_id = Some(step_id);
        self
    }

    /// 创建高风险警告
    pub fn high_risk(message: String) -> Self {
        Self::new(ValidationWarningType::HighRisk, message)
    }

    /// 创建复杂依赖警告
    pub fn complex_dependency(message: String) -> Self {
        Self::new(ValidationWarningType::ComplexDependency, message)
    }

    /// 创建长执行时间警告
    pub fn long_execution_time(message: String) -> Self {
        Self::new(ValidationWarningType::LongExecutionTime, message)
    }

    /// 创建步骤过多警告
    pub fn many_steps(message: String) -> Self {
        Self::new(ValidationWarningType::ManySteps, message)
    }
}

impl DependencyCheckResult {
    /// 创建有效的依赖检查结果
    pub fn valid() -> Self {
        Self {
            valid: true,
            circular_dependencies: Vec::new(),
            missing_dependencies: Vec::new(),
        }
    }

    /// 创建无效的依赖检查结果
    pub fn invalid(
        circular_dependencies: Vec<Vec<String>>,
        missing_dependencies: Vec<String>,
    ) -> Self {
        Self {
            valid: false,
            circular_dependencies,
            missing_dependencies,
        }
    }

    /// 添加循环依赖
    pub fn add_circular_dependency(&mut self, cycle: Vec<String>) {
        self.circular_dependencies.push(cycle);
        self.valid = false;
    }

    /// 添加缺失依赖
    pub fn add_missing_dependency(&mut self, dependency: String) {
        self.missing_dependencies.push(dependency);
        self.valid = false;
    }

    /// 检查是否有循环依赖
    pub fn has_circular_dependencies(&self) -> bool {
        !self.circular_dependencies.is_empty()
    }

    /// 检查是否有缺失依赖
    pub fn has_missing_dependencies(&self) -> bool {
        !self.missing_dependencies.is_empty()
    }
}

impl PlanValidationResult {
    /// 创建有效的计划验证结果
    pub fn valid(risk_level: RiskLevel) -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
            risk_level,
        }
    }

    /// 创建无效的计划验证结果
    pub fn invalid(errors: Vec<ValidationError>, risk_level: RiskLevel) -> Self {
        Self {
            valid: false,
            errors,
            warnings: Vec::new(),
            risk_level,
        }
    }

    /// 添加错误
    pub fn add_error(&mut self, error: ValidationError) {
        self.errors.push(error);
        self.valid = false;
    }

    /// 添加警告
    pub fn add_warning(&mut self, warning: ValidationWarning) {
        self.warnings.push(warning);
    }

    /// 检查是否有错误
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// 检查是否有警告
    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }

    /// 获取错误数量
    pub fn error_count(&self) -> usize {
        self.errors.len()
    }

    /// 获取警告数量
    pub fn warning_count(&self) -> usize {
        self.warnings.len()
    }
}
