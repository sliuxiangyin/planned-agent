//! 事件 → 快照的归并。
//!
//! [`apply_event`] 是**纯函数**（只改一份 `&mut RunSnapshot`），与
//! [`RunStore`](super::store::RunStore)、服务循环解耦，因此可脱离 tokio / dioxus 单测。
//!
//! 终态收尾（清取消通道）由服务循环负责；本函数只做事件自身的语义。

use crate::flexible::event::PlanRunEvent;

use super::types::{now_ms, RunSnapshot, RunStatus, StepPhase, StepSnapshot, StepTrackLine};

/// 把一个执行事件并入快照。
///
/// 前置：调用方已按 `run_id` 丢弃过期事件（见 [`super::core::RunServiceCore`] 的循环）。
pub fn apply_event(snapshot: &mut RunSnapshot, event: PlanRunEvent) {
    match event {
        PlanRunEvent::RunStarted { total_steps } => {
            snapshot.total_steps = total_steps;
            snapshot.ensure_steps(total_steps);
        }
        PlanRunEvent::StepStarted { index, intent } => {
            snapshot.current_step = Some(index);
            let step = snapshot.step_or_insert(index);
            step.intent = intent;
            step.phase = StepPhase::Running;
        }
        // 当前 LLM 非流式：一轮一块文本，逐条累积进该步的轨迹（think box 要看到完整推理过程）。
        PlanRunEvent::StepThought { index, text, .. } => {
            snapshot
                .step_or_insert(index)
                .track
                .push(StepTrackLine::Thought { text });
        }
        PlanRunEvent::StepToolCall {
            index, tool, args, ok, ..
        } => {
            let step = snapshot.step_or_insert(index);
            step.tool_calls += 1;
            step.track.push(StepTrackLine::Tool { tool, args, ok });
        }
        PlanRunEvent::StepFinished { index, record } => {
            let step = snapshot.step_or_insert(index);
            // 轨迹是逐条攒出来的，而 `from_record` 会**整体覆盖**这一步 ——
            // 覆盖前先接住它，否则执行一结束 think box 就空了。
            let track = std::mem::take(&mut step.track);
            *step = StepSnapshot::from_record(&record);
            step.track = track;
        }
        PlanRunEvent::RunFinished { report } => {
            snapshot.status = if report.success {
                RunStatus::Succeeded
            } else {
                RunStatus::Failed
            };
            // 用报告整体覆盖步骤：执行器对「intent 展开失败」与「跳过」的步骤
            // **不发** `StepFinished`，只有报告里才有它们的最终相位（否则会永远停在 Pending）。
            // 报告没带步骤时保留现有步，别把已展示的进度清掉。
            if !report.steps.is_empty() {
                snapshot.total_steps = report.steps.len();
                // 同上：报告会整体重建步骤，先把已攒下的轨迹按 index 收好再重建。
                let mut tracks = std::mem::take(&mut snapshot.steps)
                    .into_iter()
                    .map(|step| (step.index, step.track))
                    .collect::<std::collections::HashMap<_, _>>();
                snapshot.steps = report
                    .steps
                    .iter()
                    .map(|record| {
                        let mut step = StepSnapshot::from_record(record);
                        if let Some(track) = tracks.remove(&step.index) {
                            step.track = track;
                        }
                        step
                    })
                    .collect::<Vec<_>>();
            }
            snapshot.report = Some(report);
            snapshot.finished_at_ms = Some(now_ms());
        }
        PlanRunEvent::Failed {
            index: Some(index),
            error,
        } => {
            let step = snapshot.step_or_insert(index);
            step.phase = StepPhase::Failed;
            step.error = Some(error);
        }
        PlanRunEvent::Failed {
            index: None,
            error,
        } => {
            snapshot.status = RunStatus::Failed;
            snapshot.error = Some(error);
            snapshot.finished_at_ms = Some(now_ms());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flexible::event::PlanRunEvent;
    use crate::flexible::report::{
        CallUsage, PlanRunReport, StepRunRecord, StepStatus,
    };
    use crate::flexible::template::{FlexiblePlanTemplate, PlanStep};

    /// 两步模板（第二步依赖第一步）。
    fn template() -> FlexiblePlanTemplate {
        FlexiblePlanTemplate {
            output_schema: None,
            task: "维护文件".to_string(),
            inputs: vec![],
            steps: vec![
                PlanStep {
                    result_reference: "#E1".to_string(),
                    intent: "读取 ${path}".to_string(),
                    expected_output: "文件内容".to_string(),
                    dependencies: vec![],
                },
                PlanStep {
                    result_reference: "#E2".to_string(),
                    intent: "追加一行".to_string(),
                    expected_output: "追加完成".to_string(),
                    dependencies: vec!["#E1".to_string()],
                },
            ],
        }
    }

    fn record(index: usize, status: StepStatus, error: Option<&str>) -> StepRunRecord {
        StepRunRecord {
            index,
            result_reference: format!("#E{index}"),
            intent: format!("第 {index} 步（展开后）"),
            expected_output: "o".to_string(),
            status,
            duration_ms: 12,
            prompt_tokens: 10,
            completion_tokens: 3,
            tool_calls: 1,
            rounds: 1,
            call_usages: vec![CallUsage {
                round: 1,
                prompt_tokens: 10,
                completion_tokens: 3,
            }],
            output_summary: None,
            error: error.map(str::to_string),
        }
    }

    fn snapshot() -> RunSnapshot {
        RunSnapshot::started("s1", 1, &template())
    }

    #[test]
    fn started_snapshot_is_all_pending() {
        let snapshot = snapshot();
        assert_eq!(snapshot.status, RunStatus::Running);
        assert_eq!(snapshot.total_steps, 2);
        assert_eq!(snapshot.progressed(), 0);
        assert_eq!(snapshot.phase_of(0), StepPhase::Pending);
        assert_eq!(snapshot.phase_of(1), StepPhase::Pending);
        // 未展开的 intent 先按模板原文展示
        assert_eq!(snapshot.steps[0].intent, "读取 ${path}");
    }

    #[test]
    fn step_events_move_progress_forward() {
        let mut snapshot = snapshot();

        apply_event(
            &mut snapshot,
            PlanRunEvent::StepStarted {
                index: 1,
                intent: "读取 a.txt".to_string(),
            },
        );
        assert_eq!(snapshot.current_step, Some(1));
        assert_eq!(snapshot.phase_of(0), StepPhase::Running);
        assert_eq!(snapshot.steps[0].intent, "读取 a.txt", "展开版应覆盖原文");

        apply_event(
            &mut snapshot,
            PlanRunEvent::StepToolCall {
                index: 1,
                tool: "read_file".to_string(),
                args: "C:/tmp/a.txt".to_string(),
                ok: true,
            },
        );
        assert_eq!(snapshot.steps[0].tool_calls, 1);

        apply_event(
            &mut snapshot,
            PlanRunEvent::StepThought {
                index: 1,
                round: 1,
                text: "先读文件".to_string(),
            },
        );

        apply_event(
            &mut snapshot,
            PlanRunEvent::StepFinished {
                index: 1,
                record: record(1, StepStatus::Done, None),
            },
        );
        assert_eq!(snapshot.phase_of(0), StepPhase::Done);
        assert_eq!(snapshot.progressed(), 1);
        assert_eq!(snapshot.steps[0].tool_calls, 1, "记录里的计数应与累计一致");
        assert_eq!(snapshot.steps[0].duration_ms, 12);
        // 轨迹按发生顺序累积，且必须**扛住 `from_record` 的整体覆盖** ——
        // `StepFinished` 走的就是整体覆盖那条路，不接住它 think box 一执行完就空。
        assert_eq!(
            snapshot.steps[0].track,
            vec![
                StepTrackLine::Tool {
                    tool: "read_file".to_string(),
                    args: "C:/tmp/a.txt".to_string(),
                    ok: true,
                },
                StepTrackLine::Thought {
                    text: "先读文件".to_string(),
                },
            ],
            "轨迹应累积，并在 StepFinished 覆盖后仍在"
        );
    }

    #[test]
    fn run_finished_overwrites_steps_from_report() {
        let mut snapshot = snapshot();
        apply_event(
            &mut snapshot,
            PlanRunEvent::StepThought {
                index: 1,
                round: 1,
                text: "先读文件".to_string(),
            },
        );
        // 只发第一次完成：第二步跳过时执行器**不发** StepFinished
        apply_event(
            &mut snapshot,
            PlanRunEvent::StepFinished {
                index: 1,
                record: record(1, StepStatus::Done, None),
            },
        );
        assert_eq!(snapshot.phase_of(1), StepPhase::Pending);

        let report = PlanRunReport {
            success: false,
            total_duration_ms: 30,
            prompt_tokens: 20,
            completion_tokens: 6,
            tool_calls: 2,
            steps: vec![
                record(1, StepStatus::Done, None),
                record(2, StepStatus::Skipped, Some("前序步骤失败")),
            ],
        };
        apply_event(
            &mut snapshot,
            PlanRunEvent::RunFinished {
                report: report.clone(),
            },
        );

        assert_eq!(snapshot.status, RunStatus::Failed);
        assert_eq!(snapshot.phase_of(1), StepPhase::Skipped, "跳过步只能由报告补上");
        assert_eq!(snapshot.steps[1].error.as_deref(), Some("前序步骤失败"));
        assert!(snapshot.finished_at_ms.is_some());
        assert_eq!(snapshot.report.as_ref(), Some(&report));
        // 报告重建步骤时轨迹要按 index 接回来，不能随重建一起丢
        assert_eq!(
            snapshot.steps[0].track,
            vec![StepTrackLine::Thought {
                text: "先读文件".to_string(),
            }],
            "RunFinished 重建步骤后轨迹不能丢"
        );
    }

    #[test]
    fn step_failure_marks_only_that_step() {
        let mut snapshot = snapshot();
        apply_event(
            &mut snapshot,
            PlanRunEvent::Failed {
                index: Some(1),
                error: "展开失败：缺少 ${path}".to_string(),
            },
        );
        assert_eq!(snapshot.phase_of(0), StepPhase::Failed);
        assert_eq!(
            snapshot.steps[0].error.as_deref(),
            Some("展开失败：缺少 ${path}")
        );
        assert_eq!(snapshot.status, RunStatus::Running, "单步失败不结束整次");
    }

    #[test]
    fn run_level_failure_finishes_snapshot() {
        let mut snapshot = snapshot();
        apply_event(
            &mut snapshot,
            PlanRunEvent::Failed {
                index: None,
                error: "模板非法".to_string(),
            },
        );
        assert_eq!(snapshot.status, RunStatus::Failed);
        assert_eq!(snapshot.error.as_deref(), Some("模板非法"));
        assert!(snapshot.finished_at_ms.is_some());
    }

    #[test]
    fn unknown_step_index_is_inserted_in_order() {
        let mut snapshot = RunSnapshot::started("s1", 1, &template());
        snapshot.steps.clear();
        snapshot.total_steps = 0;

        apply_event(
            &mut snapshot,
            PlanRunEvent::StepStarted {
                index: 2,
                intent: "第二步".to_string(),
            },
        );

        assert_eq!(snapshot.steps.len(), 2);
        assert_eq!(snapshot.steps[0].index, 1);
        assert_eq!(snapshot.steps[0].phase, StepPhase::Pending);
        assert_eq!(snapshot.steps[1].phase, StepPhase::Running);
        assert_eq!(snapshot.total_steps, 2);
    }
}
