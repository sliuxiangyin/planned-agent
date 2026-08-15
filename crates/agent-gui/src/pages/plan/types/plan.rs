//! 计划元数据：参数定义、DB 基本信息。

/// 固化的计划参数定义（来自清晰度检查阶段 multi_select 勾选）。
///
/// 序列化为 JSON 数组存入 `plans_flexible.params`，
/// 供下次执行时渲染参数输入表单与注入 context。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ParamDef {
    /// 参数名（MultiSelect 选项 id，如 "keyword"）
    pub name: String,
    /// 参数描述（选项 label 中 "=" 左侧，如 "搜索关键词"）
    pub description: String,
    /// 本次固化的示例值（选项 label 中 "=" 右侧，如 "安仁乡"）
    pub example: String,
}

/// 计划基本信息（从 `plans` 表加载）。
#[derive(Clone)]
pub(crate) struct PlanInfo {
    pub(crate) name: String,
    pub(crate) mode: String,
    pub(crate) status: String,
}