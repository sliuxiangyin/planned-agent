use serde::{Deserialize, Deserializer, Serialize};
use chrono::{DateTime, Utc};
use serde_json::Value;
use crate::tool_registry::types::ToolCategory;

/// 容错解析推荐工具分类：LLM 可能输出枚举外的值（如 "Search"、"Web Scraping"），
/// 未知值直接丢弃，避免整个计划因单个字段反序列化失败而无法保存。
fn deserialize_tool_categories<'de, D>(deserializer: D) -> Result<Option<Vec<ToolCategory>>, D::Error>
where
    D: Deserializer<'de>,
{
    let values: Option<Vec<String>> = Option::deserialize(deserializer)?;
    Ok(values.map(|list| {
        list.into_iter()
            .filter_map(|s| {
                serde_json::from_value::<ToolCategory>(serde_json::Value::String(s)).ok()
            })
            .collect()
    }))
}

/// 计划复杂度
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PlanComplexity {
    Simple,
    Medium,
    Complex,
}

impl Default for PlanComplexity {
    /// 提示词不再强制模型产出该字段，缺省视为 `Simple`。
    fn default() -> Self {
        Self::Simple
    }
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

impl Default for RiskLevel {
    /// 提示词不再强制模型产出该字段，缺省视为 `Low`（只读、无副作用）。
    fn default() -> Self {
        Self::Low
    }
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
///
/// 提示词只要求模型产出四个核心字段（`intent` / `expected_output` /
/// `result_reference` / `dependencies`），其余字段均由
/// [`CoarseGrainedPlan::normalize`] 兜底。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoarseGrainedStep {
    /// 步骤 ID（可省略，缺省由 `normalize` 按位置补 `step-N`）
    #[serde(default)]
    pub id: String,
    /// 步骤顺序（可省略，缺省由 `normalize` 按位置补 1..n）
    #[serde(default)]
    pub order: u32,
    /// 意图描述，如："获取搜索结果"
    pub intent: String,
    /// 预期输出描述（含验收条件）
    pub expected_output: String,
    /// 结果引用标识，如："#E1"
    pub result_reference: String,
    /// 依赖的步骤结果引用列表（可省略，缺省视为无依赖）
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// 数据需求（可省略，缺省为空；前置条件应写入 `intent`）
    #[serde(default)]
    pub data_requirements: Vec<DataRequirement>,
    /// 推荐的工具分类（可省略）
    /// LLM 可能输出枚举外的值，容错反序列化：未知值丢弃，不阻塞整个计划保存
    #[serde(default, deserialize_with = "deserialize_tool_categories")]
    pub recommended_tool_categories: Option<Vec<ToolCategory>>,
}

/// 粗粒度计划
///
/// 提示词只需产出 `steps`（每步四个核心字段），其余字段均由
/// [`CoarseGrainedPlan::normalize`] 兜底，因此极简 JSON 即可解析成功。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoarseGrainedPlan {
    /// 计划 ID（可省略，缺省由 `normalize` 生成）
    #[serde(default)]
    pub id: String,
    /// 计划标题（可省略，缺省取首个步骤的 intent）
    #[serde(default)]
    pub title: String,
    /// 计划描述（可省略，缺省与 `title` 相同）
    #[serde(default)]
    pub description: String,
    /// 粗粒度步骤列表（唯一必填字段）
    pub steps: Vec<CoarseGrainedStep>,
    /// 创建时间
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
    /// 计划复杂度（可省略，缺省 `Simple`）
    #[serde(default)]
    pub complexity: PlanComplexity,
    /// 风险等级（可省略，缺省 `Low`）
    #[serde(default)]
    pub risk_level: RiskLevel,
    /// 输出格式描述（从 AI 响应中提取，执行完毕后按此 schema 产出最终结果）
    #[serde(default)]
    pub output_schema: Option<String>,
    /// 输入参数定义（JSON 承载）。
    ///
    /// 计划执行所需的输入参数以 JSON 对象描述（如 `{"keyword": {"type": "string",
    /// "description": "搜索关键词", "example": "安仁乡"}}`）。计划保存后可供
    /// 下次执行时动态替换参数、以及多计划工作流的输入对接。
    #[serde(default)]
    pub input_schema: Option<Value>,
    /// 执行指导：灵活执行阶段记录的步骤与工具路径，
    /// 计划执行时用于参考，防止跑偏。
    #[serde(default)]
    pub execution_guide: Option<String>,
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
            output_schema: None,
            input_schema: None,
            execution_guide: None,
        }
    }

    /// 补全 LLM 未产出的派生字段。
    ///
    /// 提示词只要求模型产出 `steps[]` 的四个核心字段（`intent` /
    /// `expected_output` / `result_reference` / `dependencies`），`id`、`order`、
    /// `title`、`description`、`complexity`、`risk_level`、`data_requirements`
    /// 全部由本方法兜底，使极简 JSON 也能安全落库与执行：
    ///
    /// - `id` 空 → 生成 `plan-<时间戳>`；`title` 空 → 取首个非空步骤 `intent`；
    ///   `description` 空 → 与 `title` 相同；
    /// - 步骤 `id` 空 → `step-<序号>`（从 1 开始，保证唯一，避免日志/结果 Store 冲突）；
    ///   步骤 `order` 为 0 → 按位置补 1..n。
    ///
    /// **已给出的值一律保留**（不覆盖、不去重、不纠错）。
    pub fn normalize(&mut self) {
        if self.id.trim().is_empty() {
            self.id = format!("plan-{}", Utc::now().timestamp_millis());
        }
        if self.title.trim().is_empty() {
            self.title = self
                .steps
                .first()
                .map(|step| step.intent.trim().to_string())
                .filter(|intent| !intent.is_empty())
                .unwrap_or_else(|| "未命名计划".to_string());
        }
        if self.description.trim().is_empty() {
            self.description = self.title.clone();
        }
        for (index, step) in self.steps.iter_mut().enumerate() {
            if step.id.trim().is_empty() {
                step.id = format!("step-{}", index + 1);
            }
            if step.order == 0 {
                step.order = (index + 1) as u32;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 提示词最简形态：只有 `steps[]`，每步只有四个核心字段。
    #[test]
    fn minimal_plan_json_deserializes() {
        let json = r##"{
            "steps": [
                {
                    "result_reference": "#E1",
                    "intent": "读取 /var/log/app.log 内容",
                    "expected_output": "原始日志内容",
                    "dependencies": []
                }
            ]
        }"##;

        let plan: CoarseGrainedPlan =
            serde_json::from_str(json).expect("极简 JSON 必须能解析");

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.id, "");
        assert_eq!(plan.complexity, PlanComplexity::Simple);
        assert_eq!(plan.risk_level, RiskLevel::Low);
        assert!(plan.steps[0].id.is_empty());
        assert_eq!(plan.steps[0].order, 0);
        assert!(plan.steps[0].data_requirements.is_empty());
        assert!(plan.steps[0].recommended_tool_categories.is_none());
    }

    /// 连 `dependencies` 都可省略；`normalize` 补齐派生字段。
    #[test]
    fn normalize_fills_derived_fields() {
        let json = r##"{
            "steps": [
                {
                    "result_reference": "#E1",
                    "intent": "读取 /var/log/app.log 内容",
                    "expected_output": "原始日志内容"
                },
                {
                    "result_reference": "#E2",
                    "intent": "将日志写入 /tmp/errors.log",
                    "expected_output": "/tmp/errors.log 文件"
                }
            ]
        }"##;

        let mut plan: CoarseGrainedPlan = serde_json::from_str(json).unwrap();
        plan.normalize();

        assert!(plan.id.starts_with("plan-"));
        assert_eq!(plan.title, "读取 /var/log/app.log 内容");
        assert_eq!(plan.description, plan.title);
        assert_eq!(plan.steps[0].id, "step-1");
        assert_eq!(plan.steps[1].id, "step-2");
        assert_eq!(plan.steps[0].order, 1);
        assert_eq!(plan.steps[1].order, 2);
        assert!(plan.steps[0].dependencies.is_empty());
    }

    /// 模型已给出的值不被覆盖。
    #[test]
    fn normalize_keeps_provided_values() {
        let json = r##"{
            "id": "plan-keep",
            "title": "已给标题",
            "description": "已给描述",
            "complexity": "complex",
            "risk_level": "high",
            "steps": [
                {
                    "id": "s-9",
                    "order": 7,
                    "intent": "做一件事",
                    "expected_output": "做完了",
                    "result_reference": "#E1",
                    "dependencies": [],
                    "data_requirements": []
                }
            ]
        }"##;

        let mut plan: CoarseGrainedPlan = serde_json::from_str(json).unwrap();
        plan.normalize();

        assert_eq!(plan.id, "plan-keep");
        assert_eq!(plan.title, "已给标题");
        assert_eq!(plan.description, "已给描述");
        assert_eq!(plan.complexity, PlanComplexity::Complex);
        assert_eq!(plan.risk_level, RiskLevel::High);
        assert_eq!(plan.steps[0].id, "s-9");
        assert_eq!(plan.steps[0].order, 7);
    }

    /// 无标题时 `normalize` 有兜底值，不会留空串。
    #[test]
    fn normalize_handles_empty_steps() {
        let mut plan = CoarseGrainedPlan::new(
            String::new(),
            String::new(),
            String::new(),
            Vec::new(),
            PlanComplexity::Simple,
            RiskLevel::Low,
        );
        plan.normalize();

        assert!(plan.id.starts_with("plan-"));
        assert_eq!(plan.title, "未命名计划");
        assert_eq!(plan.description, "未命名计划");
    }
}


