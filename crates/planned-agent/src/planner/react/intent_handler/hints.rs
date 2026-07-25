//! 意图提示文案 + flags 生成
//!
//! 把 `StepIntent` 翻译成 3 个 Tera 模板变量：
//! - `has_intent_hint : bool`  — 总开关，MixedFocus 时为 `false`，模板里的
//!                                 `{% if %}` 整体短路，prompt 中不出现任何空行 / 标题。
//! - `intent_label    : String` — 中文标签（"浏览器" / "文本" / …），用于渲染标题行。
//! - `intent_hint     : String` — 提示文案（MixedFocus 时为空串）。

use std::collections::HashMap;
use serde_json::Value;
use crate::planner::react::intent_router::StepIntent;

/// 意图处理器：把 `StepIntent` 翻译成模板可消费的变量集合。
pub struct IntentHandler;

impl IntentHandler {
    /// 处理一个意图，返回 Tera 模板可直接消费的 3 个变量。
    pub fn handle(intent: StepIntent) -> HashMap<&'static str, Value> {
        let mut flags = HashMap::with_capacity(3);
        flags.insert(
            "has_intent_hint",
            Value::Bool(!matches!(intent, StepIntent::MixedFocus)),
        );
        flags.insert("intent_label", Value::String(intent.label().to_string()));
        flags.insert("intent_hint", Value::String(intent.hint().to_string()));
        flags
    }
}

impl StepIntent {
    /// 意图的中文标签（MixedFocus 为空串，避免渲染空标题）。
    pub fn label(&self) -> &'static str {
        match self {
            StepIntent::BrowserFocus => "浏览器",
            StepIntent::TextFocus => "文本",
            StepIntent::DataFocus => "数据",
            StepIntent::FileFocus => "文件",
            StepIntent::SystemFocus => "系统",
            StepIntent::DevFocus => "开发",
            StepIntent::DeviceFocus => "设备",
            StepIntent::UtilityFocus => "工具",
            StepIntent::MixedFocus => "",
        }
    }

    /// 意图的专属提示文案（MixedFocus 为空串）。
    pub fn hint(&self) -> &'static str {
        match self {
            StepIntent::BrowserFocus => BROWSER_HINT,
            StepIntent::TextFocus => TEXT_HINT,
            StepIntent::DataFocus => DATA_HINT,
            StepIntent::FileFocus => FILE_HINT,
            StepIntent::SystemFocus => SYSTEM_HINT,
            StepIntent::DevFocus => DEV_HINT,
            StepIntent::DeviceFocus => DEVICE_HINT,
            StepIntent::UtilityFocus => UTILITY_HINT,
            StepIntent::MixedFocus => "",
        }
    }
}

// =====================================================================
// 意图专属提示文案
// ---------------------------------------------------------------------
// 每段 ~80-120 tokens，简洁、可操作、对 LLM 友好。
// =====================================================================

const BROWSER_HINT: &str = r#"
- 浏览器工具返回的 HTML / 原始 DOM 严禁直接进入下一步；必须经过 page_extract 等工具结构化后再传递"#;

const TEXT_HINT: &str = r#"- 当前聚焦文本处理：优先使用 text_grep / text_transform / text_count 等文本类工具
- 输入通常是纯字符串或简单结构化文本，避免调用浏览器或 shell
- 如需语义理解（分类、摘要、改写），可使用 ai_process 工具"#;

const DATA_HINT: &str = r#"- 当前聚焦数据处理：输入通常是结构化数据（JSON / CSV / 数据库结果）
- 排序、聚合、过滤、字段映射等转换优先使用 ai_process 工具
- 输出必须保持 JSON 结构，禁止混入 Markdown 或解释性文本"#;

const FILE_HINT: &str = r#"- 当前聚焦文件操作：严格保留用户原始输入中的文件路径与文件名，禁止泛化或缩写
- 优先使用 file_read / file_write / file_list 等文件类工具
- 注意区分绝对路径与相对路径，避免越权访问工作目录之外的路径（除非用户明确授权）"#;

const SYSTEM_HINT: &str = r#"- 当前聚焦系统命令：仅在确有需要时执行 shell，避免冗余调用
- 优先使用 system_exec 类工具，命令中禁止注入未转义的用户输入
- 如果命令输出大量文本，应在下一步用 text_grep 或 ai_process 提取关键内容"#;

const DEV_HINT: &str = r#"- 当前聚焦开发操作：涉及 Git、构建、测试等任务
- 优先使用 git_* / build_* / test_* 类工具，参数严格匹配工具 schema
- 谨慎处理破坏性操作（git push / reset / force），执行前确认用户授权"#;

const DEVICE_HINT: &str = r#"- 当前聚焦设备操作：通过 ADB 与移动设备交互
- 优先使用 adb_* 类工具，避免绕开 ADB 直接操作
- 设备 ID / 包名必须原样保留，禁止泛化为示例值"#;

const UTILITY_HINT: &str = r#"- 当前聚焦工具 / 内置操作：使用工具类或自定义内置能力
- 选择最匹配意图的专用工具，避免用通用工具替代专业工具
- 注意工具的输入约束，必要时拆分多次调用"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_returns_exactly_three_keys_when_focused() {
        let flags = IntentHandler::handle(StepIntent::BrowserFocus);
        assert_eq!(flags.len(), 3, "Focused 时只输出 3 个变量");
        for k in ["has_intent_hint", "intent_label", "intent_hint"] {
            assert!(flags.contains_key(k), "缺少键 {}", k);
        }
        assert_eq!(
            flags.get("has_intent_hint").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            flags.get("intent_label").and_then(|v| v.as_str()),
            Some("浏览器")
        );
        assert_eq!(
            flags.get("intent_hint").and_then(|v| v.as_str()),
            Some(BROWSER_HINT)
        );
    }

    #[test]
    fn mixed_yields_disabled_flag_and_empty_strings() {
        let flags = IntentHandler::handle(StepIntent::MixedFocus);
        assert_eq!(
            flags.get("has_intent_hint").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            flags.get("intent_label").and_then(|v| v.as_str()),
            Some("")
        );
        assert_eq!(
            flags.get("intent_hint").and_then(|v| v.as_str()),
            Some("")
        );
    }

    #[test]
    fn every_focused_variant_produces_non_empty_label_and_hint() {
        for intent in [
            StepIntent::BrowserFocus,
            StepIntent::TextFocus,
            StepIntent::DataFocus,
            StepIntent::FileFocus,
            StepIntent::SystemFocus,
            StepIntent::DevFocus,
            StepIntent::DeviceFocus,
            StepIntent::UtilityFocus,
        ] {
            assert!(!intent.label().is_empty(), "label 应非空: {:?}", intent);
            assert!(!intent.hint().is_empty(), "hint 应非空: {:?}", intent);
            let flags = IntentHandler::handle(intent);
            assert_eq!(
                flags.get("has_intent_hint").and_then(|v| v.as_bool()),
                Some(true),
                "has_intent_hint 应为 true: {:?}",
                intent
            );
        }
    }

    #[test]
    fn label_and_hint_are_consistent() {
        // 校验所有 8 个 focused 变体的 label / hint 都非空且与 BROWSER_HINT 之类常量匹配
        assert_eq!(StepIntent::BrowserFocus.label(), "浏览器");
        assert_eq!(StepIntent::BrowserFocus.hint(), BROWSER_HINT);
        assert_eq!(StepIntent::TextFocus.label(), "文本");
        assert_eq!(StepIntent::TextFocus.hint(), TEXT_HINT);
        assert_eq!(StepIntent::DataFocus.label(), "数据");
        assert_eq!(StepIntent::DataFocus.hint(), DATA_HINT);
        assert_eq!(StepIntent::FileFocus.label(), "文件");
        assert_eq!(StepIntent::FileFocus.hint(), FILE_HINT);
        assert_eq!(StepIntent::SystemFocus.label(), "系统");
        assert_eq!(StepIntent::SystemFocus.hint(), SYSTEM_HINT);
        assert_eq!(StepIntent::DevFocus.label(), "开发");
        assert_eq!(StepIntent::DevFocus.hint(), DEV_HINT);
        assert_eq!(StepIntent::DeviceFocus.label(), "设备");
        assert_eq!(StepIntent::DeviceFocus.hint(), DEVICE_HINT);
        assert_eq!(StepIntent::UtilityFocus.label(), "工具");
        assert_eq!(StepIntent::UtilityFocus.hint(), UTILITY_HINT);
    }
}