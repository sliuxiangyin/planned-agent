//! 意图路由
//!
//! 职责：根据 `CoarseGrainedStep.recommended_tool_categories` 解析当前步骤的主导意图，
//! 输出 `StepIntent`。
//!
//! 本模块只负责"是什么意图"。提示文案 / 模板变量生成由 `intent_handler` 子模块负责。
//!
//! 设计原则：
//! - **不修改 coarse_plan.toml**：该模板已经在产出 `recommended_tool_categories`，
//!   数据流通了，无需重复。
//! - **不新增底层枚举**：直接复用 `crate::tool_registry::types::ToolCategory`。
//! - **多类别时按优先级选主导意图**：Browser > Text > Data > File > System >
//!   Dev > Device > Utility（浏览器最容易出 token 黑洞，最值得专精化）。
//! - **缺失 / 为空 → `MixedFocus`**：交给 `intent_handler` 决定是否注入提示，
//!   默认不注入任何专属文案（prompt 体积零增量）。

use planned_agent_core::planner::coarse::CoarseGrainedStep;
use planned_agent_core::tool_registry::types::ToolCategory;

/// 步骤主导意图（按行为焦点划分，与 `ToolCategory` 一一对应 + 一个兜底变体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepIntent {
    BrowserFocus,
    TextFocus,
    DataFocus,
    FileFocus,
    SystemFocus,
    DevFocus,
    DeviceFocus,
    UtilityFocus,
    /// 多个 ToolCategory 同时出现 / 推荐列表为空 / 解析失败 —— 兜底。
    MixedFocus,
}

/// 意图路由器：决定当前 `CoarseGrainedStep` 的主导意图。
pub struct IntentRouter;

impl IntentRouter {
    /// 根据当前步骤解析主导意图。
    ///
    /// 解析策略：
    /// 1. 若 `recommended_tool_categories` 为 `None` 或空 → `MixedFocus`（兜底）
    /// 2. 若只有一个类别 → 直接映射到对应 `StepIntent`
    /// 3. 若多个 → 按 [`intent_priority`] 顺序选出优先级最高的那个
    pub fn route(step: &CoarseGrainedStep) -> StepIntent {
        let cats = match &step.recommended_tool_categories {
            Some(c) if !c.is_empty() => c,
            _ => return StepIntent::MixedFocus,
        };

        for priority_cat in intent_priority() {
            if cats.contains(&priority_cat) {
                return map_category(&priority_cat);
            }
        }
        // 理论不会走到这里（intent_priority 覆盖 ToolCategory::all()），兜底
        StepIntent::MixedFocus
    }
}

/// 把 `ToolCategory` 翻译成 `StepIntent`（一一对应）。
fn map_category(cat: &ToolCategory) -> StepIntent {
    match cat {
        ToolCategory::Browser => StepIntent::BrowserFocus,
        ToolCategory::Text => StepIntent::TextFocus,
        ToolCategory::Data => StepIntent::DataFocus,
        ToolCategory::File => StepIntent::FileFocus,
        ToolCategory::System => StepIntent::SystemFocus,
        ToolCategory::Dev => StepIntent::DevFocus,
        ToolCategory::Device => StepIntent::DeviceFocus,
        ToolCategory::Utility => StepIntent::UtilityFocus,
    }
}

/// 主导意图优先级（从高到低）。
///
/// Browser 排第一：浏览器操作最容易出 token 黑洞，最值得专精化。
fn intent_priority() -> Vec<ToolCategory> {
    vec![
        ToolCategory::Browser,
        ToolCategory::Text,
        ToolCategory::Data,
        ToolCategory::File,
        ToolCategory::System,
        ToolCategory::Dev,
        ToolCategory::Device,
        ToolCategory::Utility,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use planned_agent_core::planner::coarse::CoarseGrainedStep;

    fn make_step(cats: Option<Vec<ToolCategory>>) -> CoarseGrainedStep {
        let mut s = CoarseGrainedStep::new(
            "step-1".into(),
            1,
            "test intent".into(),
            "expected".into(),
            "#E1".into(),
        );
        s.recommended_tool_categories = cats;
        s
    }

    #[test]
    fn empty_or_none_yields_mixed() {
        assert_eq!(IntentRouter::route(&make_step(None)), StepIntent::MixedFocus);
        assert_eq!(
            IntentRouter::route(&make_step(Some(vec![]))),
            StepIntent::MixedFocus
        );
    }

    #[test]
    fn single_category_maps_directly() {
        assert_eq!(
            IntentRouter::route(&make_step(Some(vec![ToolCategory::Browser]))),
            StepIntent::BrowserFocus
        );
        assert_eq!(
            IntentRouter::route(&make_step(Some(vec![ToolCategory::Text]))),
            StepIntent::TextFocus
        );
        assert_eq!(
            IntentRouter::route(&make_step(Some(vec![ToolCategory::Data]))),
            StepIntent::DataFocus
        );
        assert_eq!(
            IntentRouter::route(&make_step(Some(vec![ToolCategory::File]))),
            StepIntent::FileFocus
        );
        assert_eq!(
            IntentRouter::route(&make_step(Some(vec![ToolCategory::System]))),
            StepIntent::SystemFocus
        );
    }

    #[test]
    fn priority_picks_browser_first() {
        // [System, Browser, Text] → Browser 胜出（优先级最高）
        assert_eq!(
            IntentRouter::route(&make_step(Some(vec![
                ToolCategory::System,
                ToolCategory::Browser,
                ToolCategory::Text,
            ]))),
            StepIntent::BrowserFocus
        );
        // [File, Text] → Text 胜出（Text 优先级高于 File）
        assert_eq!(
            IntentRouter::route(&make_step(Some(vec![
                ToolCategory::File,
                ToolCategory::Text,
            ]))),
            StepIntent::TextFocus
        );
        // [Dev, System] → System 胜出
        assert_eq!(
            IntentRouter::route(&make_step(Some(vec![
                ToolCategory::Dev,
                ToolCategory::System,
            ]))),
            StepIntent::SystemFocus
        );
    }
}

    