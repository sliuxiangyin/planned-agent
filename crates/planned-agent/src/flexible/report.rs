//! 执行报告：一次 run 的汇总指标。
//!
//! 字段直接对应宿主（STATS 块）要展示的
//! `Exec time` / `Tokens` / `Tools called` / `Steps done` / `Errors`。

use serde::{Deserialize, Serialize};

/// 单步的最终状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    /// 正常完成
    Done,
    /// 执行失败（工具循环异常 / 超轮数上限）
    Failed,
    /// 未执行（前序失败或用户取消后跳过的步骤）
    Skipped,
}

/// 一次 LLM 请求的 token 用量。
///
/// 一个步骤可能发多次 LLM 请求（每轮工具循环一次），故单步持有 `Vec<CallUsage>`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallUsage {
    /// 该步内的轮次序号（从 1 开始）。
    pub round: usize,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// 单步执行记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRunRecord {
    /// 步骤序号（从 1 开始，与事件里的 `index` 一致）。
    pub index: usize,
    /// 结果引用标识，如 `#E1`。
    pub result_reference: String,
    /// 子目标描述（占位符已展开）。
    pub intent: String,
    /// 可验证产出（原样保留，未展开）。
    pub expected_output: String,
    pub status: StepStatus,
    pub duration_ms: u64,
    /// 该步所有轮次 token 之和（= `call_usages` 的 sum）。
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// 该步调用的工具次数。
    pub tool_calls: usize,
    /// 该步的工具循环轮数（= `call_usages.len()`）。
    pub rounds: usize,
    /// 单次（每轮 LLM 请求）token 明细。
    ///
    /// 循环每轮重发整个上下文，故第 N 轮的 `prompt_tokens` 含前 N-1 轮内容；
    /// 保留明细是为了诊断"上下文在哪一轮膨胀"。
    pub call_usages: Vec<CallUsage>,
    /// 该步输出的摘要（若可提取）—— 给 STATS 与后续步骤用的**短**文本。
    pub output_summary: Option<String>,
    /// 该步输出的**全文**（若可提取，[`super::step`] 的 `OUTPUT_MAX_CHARS` 封顶）。
    ///
    /// 与 `output_summary` 的分工：摘要是「紧凑传播」，全文是「结果展示 + 输出整理步的输入」。
    pub output: Option<String>,
    /// `output` 是否因超长被截断 —— 界面必须如实告知，不得假装完整。
    pub output_truncated: bool,
    /// 失败原因。
    pub error: Option<String>,
}

impl StepRunRecord {
    /// 该步的 `prompt_tokens + completion_tokens`。
    pub fn total_tokens(&self) -> u32 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// 一次完整执行的报告。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanRunReport {
    /// 是否全部步骤成功。
    pub success: bool,
    pub total_duration_ms: u64,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub tool_calls: usize,
    /// 按执行顺序排列的步骤记录（含未执行的 `Skipped` 步，以及末尾可能追加的输出整理步）。
    pub steps: Vec<StepRunRecord>,
    /// 本次执行的**最终结果**（按 `output_schema` 整理后要交付的东西）。
    ///
    /// `None` 的三种情形：任务未成功（不整理）、用户跳过输出定义且交付步没有输出、
    /// 或契约非法（此时末尾的整理步会记为 `Failed`，原因在它的 `error` 里）。
    pub result: Option<String>,
}

impl PlanRunReport {
    /// 成功的步骤数。
    pub fn steps_done(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.status == StepStatus::Done)
            .count()
    }

    /// 失败的步骤数（宿主 STATS 的 `Errors`）。
    pub fn errors(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.status == StepStatus::Failed)
            .count()
    }

    /// 总 token（prompt + completion）。
    pub fn total_tokens(&self) -> u32 {
        self.prompt_tokens + self.completion_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(index: usize, status: StepStatus) -> StepRunRecord {
        StepRunRecord {
            index,
            result_reference: format!("#E{index}"),
            intent: "i".to_string(),
            expected_output: "o".to_string(),
            status,
            duration_ms: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            tool_calls: 0,
            rounds: 0,
            call_usages: vec![],
            output_summary: None,
            output: None,
            output_truncated: false,
            error: None,
        }
    }

    #[test]
    fn counts_done_and_errors() {
        let report = PlanRunReport {
            success: false,
            total_duration_ms: 100,
            prompt_tokens: 10,
            completion_tokens: 5,
            tool_calls: 3,
            result: None,
            steps: vec![
                record(1, StepStatus::Done),
                record(2, StepStatus::Failed),
                record(3, StepStatus::Skipped),
            ],
        };
        assert_eq!(report.steps_done(), 1);
        assert_eq!(report.errors(), 1);
        assert_eq!(report.total_tokens(), 15);
    }

    #[test]
    fn step_status_serializes_lowercase() {
        let json = serde_json::to_string(&StepStatus::Done).unwrap();
        assert_eq!(json, "\"done\"");
        assert_eq!(
            serde_json::from_str::<StepStatus>("\"failed\"").unwrap(),
            StepStatus::Failed
        );
    }
}
