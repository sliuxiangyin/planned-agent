//! 灵活模式共享类型。

use serde::{Deserialize, Serialize};

/// 完整度文档：渐进填充的结构化中间状态。
///
/// 每个步骤完成后更新对应字段，未填充的字段为空字符串。
/// 格式化为 Markdown 后注入到全局上下文中，供 AI 参考。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CompletenessDoc {
    /// 需求描述（Step 1 填充：ClarityCheck 整理后的需求）
    pub requirement: String,
    /// 输入参数定义（Step 4 填充：ParamIdentify 识别的可参数化动态值）
    pub input_params: String,
    /// 执行关键步骤（Step 2 填充：Execute 的 key_steps）
    pub execution_steps: String,
    /// 工具路径（Step 2 填充：Execute 的 tool_steps）
    pub tool_paths: String,
    /// 输出格式描述（Step 3 填充：OutputSuggest 确认的 schema）
    pub output_schema: String,
}

impl CompletenessDoc {
    /// 格式化为 Markdown 文本。
    ///
    /// 空字段标注"（待填充）"。
    pub fn to_markdown(&self) -> String {
        fn or_empty(s: &str) -> &str {
            if s.is_empty() { "（待填充）" } else { s }
        }
        format!(
            "## 需求描述\n{}\n\n## 输入参数\n{}\n\n## 执行步骤\n{}\n\n## 工具路径\n{}\n\n## 输出格式\n{}",
            or_empty(&self.requirement),
            or_empty(&self.input_params),
            or_empty(&self.execution_steps),
            or_empty(&self.tool_paths),
            or_empty(&self.output_schema),
        )
    }
}

/// 执行步骤的压缩输出。
///
/// 工具调用详情不进入全局上下文，仅传递此结构。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionOutput {
    /// 执行结果数据（人类可读）
    pub execution_result: String,
    /// 人类可读的关键步骤
    pub key_steps: Vec<String>,
    /// 工具级步骤：工具名 | 关键参数 | 结果简述
    pub tool_steps: Vec<String>,
}
