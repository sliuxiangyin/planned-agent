//! UI 交互协议模型（核心层）
//!
//! Agent 通过 `request_user_action` tool call 请求一组结构化问题（`questions`），
//! 前端渲染成一张"问题卡"，用户作答后统一回传。类型定义在 core 层，供
//! planned-agent（`ChatEvent::UIActionRequest`、挂起解析）与 agent-gui（渲染）
//! 共享。
//!
//! 设计原则：
//! - **只有一种交互原语：问题 + 一组可选答案（options）**。
//! - 单选 / 多选由一个布尔 `multi` 区分；「确认 / 跳过 / 执行」用单选问题的
//!   options 表达（视觉渲染成按钮）；每题默认带一个「自定义回答」输入框兜底
//!   （`allow_input` 缺省 true，仅当 options 已穷尽时才置 false 隐藏）。
//! - 一次调用携带 **1..N（建议 ≤4）个彼此独立的并列问题**，减少打断。

use serde::{Deserialize, Serialize};

/// 一次 `request_user_action` 携带的单个问题。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UIQuestion {
    /// 短标签（小节/页签标题，建议 ≤4 字）。同一批 questions 内应唯一，
    /// 作为该题答案在回传文本中的键。
    pub header: String,
    /// 问题全文（比 header 更完整，说明需要用户做什么决定）。
    pub question: String,
    /// 用户可点的选项（2..5 个；推荐项放第一个）。
    #[serde(default)]
    pub options: Vec<UIOption>,
    /// `false`（默认）= 单选，点击即返回；`true` = 可多选，渲染为复选框，
    /// 由前端自动补一个「提交」按钮收集勾选（协议层无需构造提交项）。
    #[serde(default)]
    pub multi: bool,
    /// 本问题是否附带一个「自定义回答」自由输入框作兜底。
    ///
    /// 默认 `true`：用户对预设 options 都不满意时可输入自己的内容作为该题答案。
    /// 仅当该问题的 options 已穷尽、确不需要用户自由补充时才显式置 `false` 隐藏。
    #[serde(default = "default_true")]
    pub allow_input: bool,
}

/// serde 默认值：`allow_input` 字段缺省时视为 `true`。
fn default_true() -> bool {
    true
}

/// 问题的可选项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UIOption {
    /// 人看的展示文本（按钮文字 / 复选框标签）。
    pub label: String,
    /// 补充说明（tooltip / 副文本），可选。
    #[serde(default)]
    pub description: Option<String>,
    /// 机器用的实际数据值，可选。回传时作为该题的答案；缺省则用 `label`。
    #[serde(default)]
    pub value: Option<String>,
}
