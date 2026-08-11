//! 计划元数据：参数定义、来源模式、生成事件、DB 基本信息。

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

/// 计划来源模式。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PlanSource {
    /// 灵活模式（阶段隔离：各阶段指令 system 注入，无全局 system 模板）——执行轨迹总结
    Flexible,
    /// 周密模式（`thorough_system.toml`）——需求确认后生成
    Thorough,
}

/// 计划生成事件——用户点击"确认生成"时触发。
///
/// 由 `chat::handle_user_action` 写入，`PlanTodoView` 读取监听。
#[derive(Clone, Debug)]
pub(crate) struct PlanGeneratedEvent {
    /// LLM 输出的计划/总结文本
    pub plan_text: String,
    /// 来源 prompt 模式
    pub source: PlanSource,
    /// 已固化的参数定义（来自清晰度检查 multi_select 勾选）
    pub params: Vec<ParamDef>,
}

/// 计划基本信息（从 `plans` 表加载）。
#[derive(Clone)]
pub(crate) struct PlanInfo {
    pub(crate) name: String,
    pub(crate) mode: String,
    pub(crate) status: String,
    pub(crate) created_at: String,
}
