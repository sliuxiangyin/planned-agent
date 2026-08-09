use serde::{Deserialize, Serialize};

/// 兜底确认按钮的固定 id —— 后端组合校验（service 层补按钮）与
/// 前端渲染兜底共用，避免两处硬编码漂移。
pub const FALLBACK_CONFIRM_ID: &str = "confirm";
/// 兜底确认按钮的固定展示文本。
pub const FALLBACK_CONFIRM_LABEL: &str = "确定";

/// UI 交互动作 —— Agent 通过 tool call 请求前端渲染交互组件。
///
/// 当 `ChatService` 检测到 `request_user_action` tool call 时，
/// 会将其参数解析为此结构，并通过 `ChatEvent::UIActionRequest` 下发到前端。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UIAction {
    /// 动作唯一标识（如 "generate_plan", "add_more_detail"）
    pub id: String,
    /// 动作类型
    #[serde(rename = "type")]
    pub action_type: UIActionType,
    /// 展示文本（按钮文字、选项标签）
    pub label: String,
    /// 补充说明（可选 tooltip / 副文本）
    #[serde(default)]
    pub description: Option<String>,
    /// MultiSelect 复选框选项列表（仅 action_type = multi_select 时有效）
    #[serde(default)]
    pub options: Vec<MultiSelectOption>,
}

/// MultiSelect 复选框选项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiSelectOption {
    /// 选项唯一标识（如 "city", "keyword"）
    pub id: String,
    /// 展示文本（如 "城市"）
    pub label: String,
    /// 选项的实际数据值（推荐）。勾选后回传为 `id=value`；不填时仅回传 `id`。
    #[serde(default)]
    pub value: Option<String>,
    /// 是否默认勾选
    #[serde(default)]
    pub default: bool,
}

/// UI 交互动作类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UIActionType {
    /// 确认按钮——用于"是/否"或多选一场景（如"生成计划"/"我再想想"）
    Confirm,
    /// 单选列表——从多个选项中选一个（如选择分析类型）
    Select,
    /// 文本输入提示——引导用户输入具体信息（如路径、关键词）
    Input,
    /// 多选复选框——用于逐项勾选（如"固化哪些参数"）
    MultiSelect,
}
