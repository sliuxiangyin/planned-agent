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
    /// 同一轮内的第几次请求：`0` = 该轮首发，`n` = 该轮第 n 次**重发**（空回答重发）。
    ///
    /// 重发与首发在 `round` 上相同，所以 `call_usages.len()` **不再等于** `rounds`
    /// （见 `StepRunRecord::rounds` / `llm_retries`）。
    #[serde(default)]
    pub retry: usize,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// 一次工具调用的记录（步骤级，按发生顺序排列）。
///
/// 与快照 `track` 里的 `$` 行**同源、不同用途**：
/// - `$` 行是给人看的单行摘要（`describe_tool_args`，`args` 封顶 120 字符）；
/// - 这里是给排查/统计用的结构化记录（`describe_arguments` 全量 JSON）。
///
/// 两者**不合并**：一个求短，一个求全（见 `crates/planned-agent/src/flexible/step.rs`
/// 里「渲染一次，三处复用」的说明）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// 工具名，如 `read_file`。
    pub tool: String,
    /// 入参（JSON 文本，`describe_arguments` 渲染）。
    pub args: String,
    /// 是否失败。工具**返回错误结果**与工具**执行异常**都算 `false`。
    pub ok: bool,
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
    /// 该步调用的工具次数（= `tool_sequence.len()`）。
    pub tool_calls: usize,
    /// 该步的工具**序列**（按发生顺序排列）。
    ///
    /// C1：与 `PlanRunEvent::StepToolCall` **同一处**采集 —— 一处采集、两条出口
    /// （事件 → 快照 `track` → think box 显示；这里 → 报告）。
    /// `#[serde(default)]` 让缺失该字段的旧 JSON 仍可解析。
    #[serde(default)]
    pub tool_sequence: Vec<ToolCallRecord>,
    /// 该步的工具循环轮数（工具调用驱动的轮数）。
    ///
    /// ⚠️ 与 `call_usages.len()` 的关系：`call_usages.len() == rounds + llm_retries`
    /// （每一轮首发一条；空回答重发不占 `rounds`，只多出带 `retry > 0` 的条目）。
    pub rounds: usize,
    /// 该步发生过的「空回答重发」总次数（0 = 从未重发）。
    ///
    /// 与 `llm_timeout_retries` 是**两条独立的线**：那条在 `request_llm` 内层自我消化、
    /// 不记条目；这条每次都在 `call_usages` 里留一条。默认上限由
    /// `ExecutorConfig::llm_empty_retries` 逐轮控制。
    #[serde(default)]
    pub llm_retries: usize,
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
    /// 该步产出**落盘**时的文件路径（产出超过落盘阈值才有值）。
    ///
    /// 落盘后全文不进下游 prompt，下游以「文件说明 + 预览」引用、按需用
    /// `builtin_read_file_lines` 分批读取。见 `docs/planned-agent/flexible-step-output-spill.md`。
    #[serde(default)]
    pub output_file: Option<String>,
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
            tool_sequence: vec![],
            rounds: 0,
            llm_retries: 0,
            call_usages: vec![],
            output_summary: None,
            output: None,
            output_file: None,
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
