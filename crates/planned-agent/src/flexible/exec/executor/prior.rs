//! 前序产出收集与依赖校验。

use std::collections::{HashMap, HashSet};

use super::spill::{render_prior_output, StoredOutput};
use super::super::report::{StepRunRecord, StepStatus};
use super::super::super::plan::template::{FlexiblePlanTemplate, PlanStep}; 

/// 校验步骤依赖：每个 `dependencies` 引用的 `result_reference` 必须**已在本步之前出现**。
///
/// 一条规则同时覆盖四种模板错误：引用不存在、自依赖、依赖后面的步骤、依赖环
/// （线性执行 + 只允许引用前面 ⇒ 环必然表现为「依赖后面的步骤」）。
///
/// 返回问题描述（空 = 通过）。**调用方只警告不阻断** —— 运行期 [`collect_prior`]
/// 会再兜底一次（对未命中的引用跳过并告警）。
///
/// 为什么能静态判定：「前序失败导致没产出」不会走到 `collect_prior` ——
/// 一有步骤非 `Done`，其后每步都直接记 `Skipped`（见 `run` 的 `blocked_earlier`）。
pub(super) fn collect_dependency_issues(template: &FlexiblePlanTemplate) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::with_capacity(template.steps.len());
    let mut issues = Vec::new();
    for (offset, step) in template.steps.iter().enumerate() {
        for dependency in &step.dependencies {
            if !seen.contains(dependency.as_str()) {
                issues.push(format!(
                    "步骤 {}（{}）的依赖 {} 未在此之前出现",
                    offset + 1,
                    step.result_reference,
                    dependency
                ));
            }
        }
        seen.insert(step.result_reference.as_str());
    }
    issues
}

/// 收集某步依赖项的实际输出（按 `dependencies` 顺序）。
///
/// 未命中的引用会被跳过 —— 正常情况下不该发生（能执行到本步 ⇒ 前序全 `Done`
/// ⇒ 产出都在 store 里），走到这里只可能是模板依赖写错（`collect_dependency_issues`
/// 只警告未阻断），故留一条 warn 作痕迹。
pub(super) fn collect_prior(
    step: &PlanStep,
    store: &HashMap<String, StoredOutput>,
    preview_chars: usize,
) -> Vec<(String, String)> {
    let mut prior = Vec::with_capacity(step.dependencies.len());
    for reference in &step.dependencies {
        match store.get(reference) {
            Some(output) => prior.push((
                reference.clone(),
                render_prior_output(output, preview_chars),
            )),
            None => tracing::warn!(
                step = %step.result_reference,
                reference = %reference,
                "依赖结果不在结果表中，已跳过（模板依赖可能写错）"
            ),
        }
    }
    prior
}

/// 未执行步骤的占位记录（`Skipped` / 展开失败）。
pub(super) fn placeholder_record(
    index: usize,
    step: &PlanStep,
    status: StepStatus,
    error: Option<&str>,
) -> StepRunRecord {
    StepRunRecord {
        index,
        result_reference: step.result_reference.clone(),
        intent: step.intent.clone(),
        expected_output: step.expected_output.clone(),
        status,
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
        error: error.map(str::to_string),
    }
}
