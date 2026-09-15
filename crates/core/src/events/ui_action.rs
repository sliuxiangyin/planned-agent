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
//!   options 表达（视觉渲染成统一的选项行）；每题默认带一个「自定义答案」行兜底
//!   （`allow_input` 缺省 true，仅当 options 已穷尽时才置 false 隐藏）。
//! - 每个 `options` 项可标 `recommended: true` 作为推荐项（每题至多一个），供前端
//!   「推荐选项」按钮一键采纳。
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
    /// 用户可点的选项（2..5 个）。推荐项用 `UIOption::recommended` 标注，
    /// 不要再靠“放第一个”表达推荐。
    #[serde(default)]
    pub options: Vec<UIOption>,
    /// `false`（默认）= 单选：选项行互斥，点选即推进到下一题（**末题除外**，等点
    /// 底部「继续」提交；再点已选中那行则取消选中、不推进）；`true` = 多选：可勾选
    /// 多项，点底部「继续」推进。两者在前端共用同一套选项行样式。
    #[serde(default)]
    pub multi: bool,
    /// 本问题是否附带「自定义答案」兜底。
    ///
    /// 默认 `true`：前端在选项列表末尾附一行**行内输入框**（`placeholder` 即
    /// 「输入自定义答案」），用户对预设 options 都不满意时可点进去直接输入。
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
    /// 人看的展示文本（选项行的主文案）。
    pub label: String,
    /// 补充说明，可选；前端渲染为该选项行的灰色副文案。
    #[serde(default)]
    pub description: Option<String>,
    /// 机器用的实际数据值，可选。回传时作为该题的答案；缺省则用 `label`。
    #[serde(default)]
    pub value: Option<String>,
    /// 是否为该题的「推荐项」。
    ///
    /// 默认 `false`。为 `true` 时前端在该选项行显示「推荐」角标，并让卡片左下角的
    /// 「推荐选项」按钮可点——点击即采纳该推荐（单选 = 选中并推进、多选 = 勾选）。
    /// 每题**至多一个**；模型没有把握时全部留空，此时按钮置灰不可点。
    #[serde(default)]
    pub recommended: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `options[].recommended` 缺省视为 `false`，显式 `true` 时能读到；
    /// 旧数据（不含该字段）反序列化不受影响。
    #[test]
    fn option_recommended_defaults_false_and_parses_true() {
        let q: UIQuestion = serde_json::from_value(json!({
            "header": "午餐",
            "question": "今天午餐想吃什么？",
            "multi": false,
            "options": [
                { "label": "热辣过瘾，适合多人聚餐", "value": "spicy", "recommended": true },
                { "label": "健康轻食，适合减脂" }
            ]
        }))
        .expect("UIQuestion 反序列化应成功");

        assert_eq!(q.options.len(), 2);
        assert!(q.options[0].recommended, "显式 true 应被读到");
        assert!(!q.options[1].recommended, "缺省应为 false");
        assert!(q.allow_input, "allow_input 缺省应为 true");
        assert_eq!(q.options[1].value, None);
    }

    /// 序列化一轮回来仍是同一个 recommended（前端/服务端共享该字段）。
    #[test]
    fn option_recommended_round_trips() {
        let q = UIQuestion {
            header: "格式".to_string(),
            question: "输出格式是？".to_string(),
            options: vec![UIOption {
                label: "CSV".to_string(),
                description: Some("适合 Excel 打开".to_string()),
                value: Some("csv".to_string()),
                recommended: true,
            }],
            multi: true,
            allow_input: true,
        };
        let text = serde_json::to_string(&q).expect("序列化应成功");
        let back: UIQuestion = serde_json::from_str(&text).expect("反序列化应成功");
        assert_eq!(back, q);
    }
}
