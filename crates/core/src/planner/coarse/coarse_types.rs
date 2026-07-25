use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use crate::tool_registry::types::ToolCategory;

/// 计划复杂度
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PlanComplexity {
    Simple,
    Medium,
    Complex,
}

/// 风险等级
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// 数据需求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataRequirement {
    /// 需求名称
    pub name: String,
    /// 需求描述
    pub description: String,
    /// 是否必需
    pub required: bool,
    /// 来源提示，如："从搜索结果中提取"
    pub source_hint: String,
}

/// 粗粒度步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoarseGrainedStep {
    /// 步骤 ID
    pub id: String,
    /// 步骤顺序
    pub order: u32,
    /// 意图描述，如："获取搜索结果"
    pub intent: String,
    /// 预期输出描述
    pub expected_output: String,
    /// 结果引用标识，如："#E1"
    pub result_reference: String,
    /// 依赖的步骤结果引用列表
    pub dependencies: Vec<String>,
    /// 数据需求
    pub data_requirements: Vec<DataRequirement>,
    /// 推荐的工具分类（可选，用于分类过滤）
    #[serde(default)]
    pub recommended_tool_categories: Option<Vec<ToolCategory>>,
}

/// 粗粒度计划
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoarseGrainedPlan {
    /// 计划 ID
    pub id: String,
    /// 计划标题
    pub title: String,
    /// 计划描述
    pub description: String,
    /// 粗粒度步骤列表
    pub steps: Vec<CoarseGrainedStep>,
    /// 创建时间
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
    /// 计划复杂度
    pub complexity: PlanComplexity,
    /// 风险等级
    pub risk_level: RiskLevel,
}

/// 计划验证结果
#[derive(Debug, Clone)]
pub struct CoarsePlanValidationResult {
    /// 是否有效
    pub valid: bool,
    /// 错误列表
    pub errors: Vec<String>,
    /// 警告列表
    pub warnings: Vec<String>,
}

impl CoarseGrainedPlan {
    /// 创建新的粗粒度计划
    pub fn new(
        id: String,
        title: String,
        description: String,
        steps: Vec<CoarseGrainedStep>,
        complexity: PlanComplexity,
        risk_level: RiskLevel,
    ) -> Self {
        Self {
            id,
            title,
            description,
            steps,
            created_at: Utc::now(),
            complexity,
            risk_level,
        }
    }

    /// 获取计划步骤数量
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }
}

impl CoarseGrainedStep {
    /// 创建新的粗粒度步骤
    pub fn new(
        id: String,
        order: u32,
        intent: String,
        expected_output: String,
        result_reference: String,
    ) -> Self {
        Self {
            id,
            order,
            intent,
            expected_output,
            result_reference,
            dependencies: Vec::new(),
            data_requirements: Vec::new(),
            recommended_tool_categories: None,
        }
    }

    /// 添加依赖
    pub fn with_dependency(mut self, reference: String) -> Self {
        self.dependencies.push(reference);
        self
    }

    /// 添加数据需求
    pub fn with_data_requirement(mut self, requirement: DataRequirement) -> Self {
        self.data_requirements.push(requirement);
        self
    }

    /// 设置推荐的工具分类
    pub fn with_tool_categories(mut self, categories: Vec<ToolCategory>) -> Self {
        self.recommended_tool_categories = Some(categories);
        self
    }
}

impl DataRequirement {
    /// 创建新的数据需求
    pub fn new(
        name: String,
        description: String,
        required: bool,
        source_hint: String,
    ) -> Self {
        Self {
            name,
            description,
            required,
            source_hint,
        }
    }
}


