//! 输出整理步（#RESULT）。

use std::collections::HashMap;

use super::super::spill::StoredOutput;
use super::super::report::{StepRunRecord, StepStatus};
use super::super::super::plan::template::FlexiblePlanTemplate; 

/// 输出整理步的结果引用标识（它不是模板步骤，标签固定）。
pub(crate) const RESOLVE_RESULT_REFERENCE: &str = "#RESULT";

/// 交付步的输出 —— 即最后一个「拿到了输出」的模板步骤。
///
/// 模板是粗粒度**线性骨架**，最后一步就是交付步（依赖图「出度 0」的判定在该形态下与
/// 「最后一步」等价，所以不引入依赖图分析）。失败 / 跳过的步骤不会进 `store`
/// （只有真拿到 output 才写入），所以这里天然只会取到有产出的那一步。
pub(super) fn deliverable_output<'t, 's>(
    template: &'t FlexiblePlanTemplate,
    store: &'s HashMap<String, StoredOutput>,
) -> Option<(&'t str, &'s StoredOutput)> {
    template.steps.iter().rev().find_map(|step| {
        store
            .get(&step.result_reference)
            .map(|output| (step.result_reference.as_str(), output))
    })
}

/// 契约非法时补一条 `Failed` 的整理步记录（让「为什么没有结果」在报告里可见）。
pub(super) fn resolve_failed_record(index: usize, error: String) -> StepRunRecord {
    StepRunRecord {
        index,
        result_reference: RESOLVE_RESULT_REFERENCE.to_string(),
        intent: "按输出契约整理本次执行的最终结果".to_string(),
        expected_output: "（输出契约非法，未执行整理）".to_string(),
        status: StepStatus::Failed,
        duration_ms: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        tool_calls: 0,
        tool_sequence: vec![],
        rounds: 0,
        llm_retries: 0,
        call_usages: vec![],
        output_summary: None,
        output: None,
        output_truncated: false,
        output_file: None,
        error: Some(error),
    }
}
