use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

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
    pub created_at: DateTime<Utc>,
    /// 计划复杂度
    pub complexity: PlanComplexity,
    /// 预估执行时间（毫秒）
    pub estimated_duration_ms: u64,
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
            estimated_duration_ms: 0,
            risk_level,
        }
    }

    /// 获取计划步骤数量
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// 检查是否有循环依赖
    pub fn has_circular_dependencies(&self) -> bool {
        use std::collections::{HashMap, HashSet};

        // 构建依赖图：result_reference -> dependencies
        let mut graph: HashMap<String, Vec<String>> = HashMap::new();
        for step in &self.steps {
            graph.insert(step.result_reference.clone(), step.dependencies.clone());
        }

        // DFS 检测环
        fn has_cycle(
            node: &str,
            graph: &HashMap<String, Vec<String>>,
            visited: &mut HashSet<String>,
            in_stack: &mut HashSet<String>,
        ) -> bool {
            if in_stack.contains(node) {
                return true; // 发现环
            }
            if visited.contains(node) {
                return false; // 已访问过，无环
            }

            visited.insert(node.to_string());
            in_stack.insert(node.to_string());

            if let Some(deps) = graph.get(node) {
                for dep in deps {
                    if has_cycle(dep, graph, visited, in_stack) {
                        return true;
                    }
                }
            }

            in_stack.remove(node);
            false
        }

        let mut visited = HashSet::new();
        let mut in_stack = HashSet::new();

        for step in &self.steps {
            if has_cycle(&step.result_reference, &graph, &mut visited, &mut in_stack) {
                return true;
            }
        }

        false
    }

    /// 检查 result_reference 是否唯一
    pub fn has_unique_references(&self) -> bool {
        use std::collections::HashSet;
        let mut refs = HashSet::new();
        for step in &self.steps {
            if !refs.insert(&step.result_reference) {
                return false;
            }
        }
        true
    }

    /// 验证计划合法性
    pub fn validate(&self) -> CoarsePlanValidationResult {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        // 检查标题和描述
        if self.title.is_empty() {
            errors.push("计划标题不能为空".to_string());
        }
        if self.description.is_empty() {
            warnings.push("计划描述为空".to_string());
        }

        // 检查步骤
        if self.steps.is_empty() {
            errors.push("计划必须包含至少一个步骤".to_string());
        }

        // 检查 result_reference 唯一性
        if !self.has_unique_references() {
            errors.push("步骤的 result_reference 必须唯一".to_string());
        }

        // 检查循环依赖
        if self.has_circular_dependencies() {
            errors.push("计划存在循环依赖".to_string());
        }

        // 检查依赖引用是否存在
        let refs: std::collections::HashSet<String> = self.steps.iter().map(|s| s.result_reference.clone()).collect();
        for step in &self.steps {
            for dep in &step.dependencies {
                if !refs.contains(dep) {
                    errors.push(format!(
                        "步骤 {} 依赖的 {} 不存在",
                        step.result_reference, dep
                    ));
                }
                // 检查不能依赖自己
                if dep == &step.result_reference {
                    errors.push(format!(
                        "步骤 {} 不能依赖自己",
                        step.result_reference
                    ));
                }
            }
        }

        // 检查 order 连续性
        let mut orders: Vec<u32> = self.steps.iter().map(|s| s.order).collect();
        orders.sort();
        for (i, &order) in orders.iter().enumerate() {
            if order as usize != i + 1 {
                warnings.push(format!("步骤顺序不连续，期望 {} 实际 {}", i + 1, order));
                break;
            }
        }

        CoarsePlanValidationResult {
            valid: errors.is_empty(),
            errors,
            warnings,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_plan() -> CoarseGrainedPlan {
        let step1 = CoarseGrainedStep::new(
            "step-1".to_string(),
            1,
            "搜索相关文档".to_string(),
            "获取搜索结果列表".to_string(),
            "#E1".to_string(),
        );

        let step2 = CoarseGrainedStep::new(
            "step-2".to_string(),
            2,
            "提取关键信息".to_string(),
            "提取文档中的关键信息".to_string(),
            "#E2".to_string(),
        )
        .with_dependency("#E1".to_string());

        CoarseGrainedPlan::new(
            "plan-001".to_string(),
            "测试计划".to_string(),
            "这是一个测试计划".to_string(),
            vec![step1, step2],
            PlanComplexity::Medium,
            RiskLevel::Low,
        )
    }

    #[test]
    fn test_plan_serialization() {
        let plan = create_test_plan();
        
        // 序列化
        let json = serde_json::to_string(&plan).unwrap();
        assert!(json.contains("plan-001"));
        assert!(json.contains("测试计划"));
        
        // 反序列化
        let deserialized: CoarseGrainedPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, plan.id);
        assert_eq!(deserialized.title, plan.title);
        assert_eq!(deserialized.steps.len(), plan.steps.len());
    }

    #[test]
    fn test_validate_valid_plan() {
        let plan = create_test_plan();
        let result = plan.validate();
        assert!(result.valid);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_validate_empty_title() {
        let mut plan = create_test_plan();
        plan.title = "".to_string();
        let result = plan.validate();
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("标题不能为空")));
    }

    #[test]
    fn test_validate_no_steps() {
        let mut plan = create_test_plan();
        plan.steps = Vec::new();
        let result = plan.validate();
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("至少一个步骤")));
    }

    #[test]
    fn test_validate_duplicate_references() {
        let step1 = CoarseGrainedStep::new(
            "step-1".to_string(),
            1,
            "步骤1".to_string(),
            "输出1".to_string(),
            "#E1".to_string(),
        );
        let step2 = CoarseGrainedStep::new(
            "step-2".to_string(),
            2,
            "步骤2".to_string(),
            "输出2".to_string(),
            "#E1".to_string(), // 重复的 reference
        );

        let plan = CoarseGrainedPlan::new(
            "plan-001".to_string(),
            "测试计划".to_string(),
            "这是一个测试计划".to_string(),
            vec![step1, step2],
            PlanComplexity::Simple,
            RiskLevel::Low,
        );

        let result = plan.validate();
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("唯一")));
    }

    #[test]
    fn test_validate_circular_dependency() {
        let step1 = CoarseGrainedStep::new(
            "step-1".to_string(),
            1,
            "步骤1".to_string(),
            "输出1".to_string(),
            "#E1".to_string(),
        )
        .with_dependency("#E2".to_string());

        let step2 = CoarseGrainedStep::new(
            "step-2".to_string(),
            2,
            "步骤2".to_string(),
            "输出2".to_string(),
            "#E2".to_string(),
        )
        .with_dependency("#E1".to_string());

        let plan = CoarseGrainedPlan::new(
            "plan-001".to_string(),
            "测试计划".to_string(),
            "这是一个测试计划".to_string(),
            vec![step1, step2],
            PlanComplexity::Simple,
            RiskLevel::Low,
        );

        assert!(plan.has_circular_dependencies());
        let result = plan.validate();
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("循环依赖")));
    }

    #[test]
    fn test_validate_self_dependency() {
        let step1 = CoarseGrainedStep::new(
            "step-1".to_string(),
            1,
            "步骤1".to_string(),
            "输出1".to_string(),
            "#E1".to_string(),
        )
        .with_dependency("#E1".to_string()); // 自己依赖自己

        let plan = CoarseGrainedPlan::new(
            "plan-001".to_string(),
            "测试计划".to_string(),
            "这是一个测试计划".to_string(),
            vec![step1],
            PlanComplexity::Simple,
            RiskLevel::Low,
        );

        let result = plan.validate();
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("不能依赖自己")));
    }

    #[test]
    fn test_validate_nonexistent_dependency() {
        let step1 = CoarseGrainedStep::new(
            "step-1".to_string(),
            1,
            "步骤1".to_string(),
            "输出1".to_string(),
            "#E1".to_string(),
        )
        .with_dependency("#E99".to_string()); // 不存在的依赖

        let plan = CoarseGrainedPlan::new(
            "plan-001".to_string(),
            "测试计划".to_string(),
            "这是一个测试计划".to_string(),
            vec![step1],
            PlanComplexity::Simple,
            RiskLevel::Low,
        );

        let result = plan.validate();
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("不存在")));
    }

    #[test]
    fn test_has_unique_references() {
        let plan = create_test_plan();
        assert!(plan.has_unique_references());
    }

    #[test]
    fn test_has_circular_dependencies() {
        let plan = create_test_plan();
        assert!(!plan.has_circular_dependencies());
    }

    #[test]
    fn test_step_count() {
        let plan = create_test_plan();
        assert_eq!(plan.step_count(), 2);
    }
}
