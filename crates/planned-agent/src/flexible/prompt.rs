//! 执行器的内置提示词常量。
//!
//! 刻意不依赖 `prompt-manager` 的文件目录：宿主无需为执行器加载任何 prompt 文件。

/// 单步执行的 system prompt。
pub(crate) const STEP_SYSTEM_PROMPT: &str = "\
你是一个计划执行助手，本次只负责完成一个子目标。

规则：
- 需要外部数据或产生副作用时，调用提供的工具完成任务；不要凭空编造工具输出。
- 工具参数必须来自「本次子目标」「期望产出」或「前序步骤结果」，禁止臆造路径、URL、关键词。
- 若已有信息足够，直接给出本次产出的结论作回答，不要再调用工具。
- 回答不要包 JSON 外壳，直接写产出内容本身。
";

/// 组装单步任务的 user 文本。
///
/// `prior` 是依赖项（`#En`）的实际输出文本，按依赖顺序排列；无依赖时为空。
pub(crate) fn build_step_task(
    intent: &str,
    expected_output: &str,
    prior: &[(String, String)],
) -> String {
    let mut text = String::new();
    text.push_str("## 本次子目标\n");
    text.push_str(intent);
    text.push_str("\n\n## 期望产出（满足什么算完成）\n");
    text.push_str(expected_output);

    if !prior.is_empty() {
        text.push_str("\n\n## 前序步骤结果\n");
        for (reference, output) in prior {
            text.push_str("### ");
            text.push_str(reference);
            text.push('\n');
            text.push_str(output);
            text.push('\n');
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_without_prior_omits_section() {
        let task = build_step_task("读取文件", "得到内容", &[]);
        assert!(task.contains("## 本次子目标\n读取文件"));
        assert!(task.contains("## 期望产出（满足什么算完成）\n得到内容"));
        assert!(!task.contains("前序步骤结果"), "无依赖时不应出现前序段");
    }

    #[test]
    fn task_with_prior_lists_references() {
        let prior = vec![
            ("#E1".to_string(), "索引=7".to_string()),
            ("#E2".to_string(), "已写入".to_string()),
        ];
        let task = build_step_task("基于 #E1 写入", "新增一行", &prior);
        assert!(task.contains("## 前序步骤结果"));
        assert!(task.contains("### #E1\n索引=7"));
        assert!(task.contains("### #E2\n已写入"));
    }
}
