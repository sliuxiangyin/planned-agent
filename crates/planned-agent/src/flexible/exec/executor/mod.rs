//! 总编排：按顺序跑每步，把前序输出递给后面的步，播事件，响应取消。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use planned_agent_core::ai::types::ToolDefinition;
use planned_agent_core::ai::AiClient;
use planned_agent_core::host::RuntimeEnvironment;
use planned_agent_core::mcp::types::Tool;
use planned_agent_tool_manager::ToolRegistry;
use tokio::sync::watch;

use super::event::{PlanRunEvent, PlanRunSink};
use super::prompt;
use super::report::{PlanRunReport, StepRunRecord, StepStatus};
use super::step::{run_output_resolve, run_step, StepInput};
use super::super::plan::output_schema::OutputSchema;
use super::super::plan::params::{render_step_expected_output, render_step_intent, PlanRunParams};
use super::super::plan::template::{FlexiblePlanTemplate, PlanStep};

use logging::{log_output, summarize_tools};
use prior::{collect_dependency_issues, collect_prior, placeholder_record};
use resolve::{deliverable_output, resolve_failed_record, RESOLVE_RESULT_REFERENCE};
use spill::{new_run_dir_name, render_prior_output, spill_output, StoredOutput};
use tools::{to_tool_definition, tool_definitions_for_names};

// ────────── 步骤产出落盘（见 docs/planned-agent/flexible-step-output-spill.md） ──────────


mod config;
mod logging;
mod prior;
mod resolve;
mod spill;
mod tools;

pub use config::{
    ExecutorConfig, DEFAULT_CACHE_DIR, DEFAULT_LLM_TIMEOUT_RETRIES, DEFAULT_LLM_TIMEOUT_SECS,
    DEFAULT_SPILL_PREVIEW_CHARS, DEFAULT_SPILL_THRESHOLD_CHARS,
};
/// 灵活计划执行器。
///
/// 与 `agent-gui` 无耦合：只认 `AiClient` 与 `ToolRegistry`，
/// 模板由调用方传入，报告返回给调用方。
pub struct FlexibleExecutor {
    ai: Arc<dyn AiClient>,
    tools: Arc<ToolRegistry>,
    cfg: ExecutorConfig,
}
impl FlexibleExecutor {
    /// 构造执行器。AI 客户端由宿主提供（如从 `AiManager` 取默认 provider）。
    pub fn new(ai: Arc<dyn AiClient>, tools: Arc<ToolRegistry>, cfg: ExecutorConfig) -> Self {
        Self { ai, tools, cfg }
    }

    /// 顺序执行整个模板，返回执行报告。
    ///
    /// - 单步失败不抛错：该步记 `Failed`，其后步骤记 `Skipped`，报告 `success = false`；
    /// - 取消同理：未执行的步骤记 `Skipped`（保留占位，UI 可显示 `N/M`）；
    /// - 展开失败（缺参数 / 未定义占位符）记为 `Failed` 并放事件，不中断整个流程。
    /// - 展开失败（缺参数 / 未定义占位符）记为 `Failed` 并放事件，不中断整个流程；
    /// - `environment` 为 `None` 时不拼环境段，system prompt 与历史行为**逐字一致**。
    pub async fn run(
        &self,
        template: &FlexiblePlanTemplate,
        params: &PlanRunParams,
        environment: Option<&RuntimeEnvironment>,
        sink: &dyn PlanRunSink,
        cancel: Option<watch::Receiver<bool>>,
    ) -> Result<PlanRunReport> {
        // 依赖校验：**只警告不阻断**（对齐「先观测、不挡路」的既定策略）。
        // 一条规则同时覆盖「引用不存在 / 自依赖 / 依赖后面的步骤 / 环」——
        // 线性执行 + 只允许引用前面 ⇒ 环必然表现为「依赖后面的步骤」。
        for issue in collect_dependency_issues(template) {
            tracing::warn!(issue = %issue, "计划依赖校验未通过（该步会拿不到前序数据）");
        }

        // 整次执行只算一次 system prompt，每步复用：同一次执行内每步字符串完全一致，
        // 使「环境段」成为可命中的 provider 前缀缓存（见 `prompt::step_system_prompt`）。
        let system_prompt = prompt::step_system_prompt(environment);
        let started = Instant::now();
        sink.emit(PlanRunEvent::RunStarted {
            total_steps: template.steps.len(),
        });
        // 执行链路原先一条日志都没有：失败原因只进事件/报告（只有 UI 看得到），
        // 事后排查日志里只能看到「底层在干活」的痕迹。这里把关键节点补齐。
        tracing::info!(
            steps = template.steps.len(),
            model = %self.ai.model_name(),
            max_rounds_per_step = self.cfg.max_rounds_per_step,
            allowed_tools = ?self.cfg.allowed_tools,
            cache_dir = %self.cfg.cache_dir.display(),
            task = %template.task.chars().take(80).collect::<String>(),
            // 整次执行只在这里打一次（每步复用同一个 system prompt，不必逐步重复）。
            // `%` 是 Display：环境段的多行会被原样输出，便于直接看清拼装结果。
            system_prompt = %system_prompt,
            "灵活计划开始执行"
        );

        let tools = self.tool_definitions();
        // 本次执行的产出缓存目录名（懒建：只有真要落盘时才 create_dir_all）。
        let run_dir = new_run_dir_name();
        let mut store: HashMap<String, StoredOutput> = HashMap::new();
        let mut records: Vec<StepRunRecord> = Vec::with_capacity(template.steps.len());

        for (offset, step) in template.steps.iter().enumerate() {
            let index = offset + 1;

            // 已取消 / 前序未全部成功 → 本步跳过（保留占位，UI 好显示 N/M）
            let blocked_earlier = records
                .iter()
                .any(|record| record.status != StepStatus::Done);
            if is_cancelled(&cancel) || blocked_earlier {
                let reason = if is_cancelled(&cancel) {
                    "用户取消"
                } else {
                    "前序步骤失败"
                };
                records.push(placeholder_record(
                    index,
                    step,
                    StepStatus::Skipped,
                    Some(reason),
                ));
                tracing::info!(step = index, reason, "步骤跳过");
                continue;
            }

            // `intent` 与 `expected_output` 都可能含 `${name}`（`placeholder::PLACEHOLDER_FIELDS`），
            // 两者都必须展开后才能交给 LLM —— 否则模型会在「期望产出」段读到未展开的 `${...}`。
            // 任一个失败都走同一条路径（该步 Failed），与「缺参数不带病执行」一致。
            let (intent, expected_output) = match (
                render_step_intent(step, params),
                render_step_expected_output(step, params),
            ) {
                (Ok(intent), Ok(expected_output)) => (intent, expected_output),
                (Err(err), _) | (_, Err(err)) => {
                    let message = err.to_string();
                    tracing::warn!(step = index, error = %message, "步骤参数展开失败，该步失败");
                    sink.emit(PlanRunEvent::Failed {
                        index: Some(index),
                        error: message.clone(),
                    });
                    records.push(placeholder_record(
                        index,
                        step,
                        StepStatus::Failed,
                        Some(message.as_str()),
                    ));
                    continue;
                }
            };

            sink.emit(PlanRunEvent::StepStarted {
                index,
                intent: intent.clone(),
            });
            tracing::info!(
                step = index,
                total = template.steps.len(),
                intent = %intent.chars().take(80).collect::<String>(),
                "步骤开始"
            );

            let prior = collect_prior(step, &store, self.cfg.spill_preview_chars);
            let result = run_step(
                &system_prompt,
                StepInput {
                    step,
                    intent: &intent,
                    expected_output: &expected_output,
                    prior: &prior,
                    tools: &tools,
                    index,
                },
                &self.ai,
                &self.tools,
                &self.cfg,
                sink,
                cancel.as_ref(),
            )
            .await;

            // 产出落盘：超阈值就写文件，下游 `prior` 只拿到「文件说明 + 预览」。
            // 落盘失败 → 该步 `Failed`（不静默降级，见设计稿 §7）。
            let mut record = result.record;
            if let Some(output) = &result.output {
                match spill_output(
                    &self.cfg.cache_dir,
                    &run_dir,
                    index,
                    self.cfg.spill_threshold_chars,
                    output,
                )
                .await
                {
                    Ok(spilled) => {
                        if let Some(spilled) = &spilled {
                            record.output_file = Some(spilled.path_string());
                            tracing::info!(
                                step = index,
                                file = %spilled.path_string(),
                                lines = spilled.lines,
                                bytes = spilled.bytes,
                                "步骤产出已落盘，下游按需读取"
                            );
                        }
                        store.insert(
                            step.result_reference.clone(),
                            StoredOutput {
                                content: output.clone(),
                                spilled,
                            },
                        );
                    }
                    Err(err) => {
                        let message = format!("产出落盘失败：{err}");
                        tracing::warn!(step = index, error = %message, "产出落盘失败，该步记为失败");
                        record.status = StepStatus::Failed;
                        record.error = Some(message);
                    }
                }
            }
            if record.status == StepStatus::Failed {
                tracing::warn!(
                    step = index,
                    rounds = record.rounds,
                    tool_calls = record.tool_calls,
                    tools = %summarize_tools(&record.tool_sequence),
                    duration_ms = record.duration_ms,
                    error = record.error.as_deref().unwrap_or("未记录原因"),
                    output_file = ?record.output_file,
                    output = %result.output.as_deref().map(log_output).unwrap_or_default(),
                    "步骤失败"
                );
            } else {
                tracing::info!(
                    step = index,
                    status = ?record.status,
                    rounds = record.rounds,
                    tool_calls = record.tool_calls,
                    tools = %summarize_tools(&record.tool_sequence),
                    duration_ms = record.duration_ms,
                    output_file = ?record.output_file,
                    output = %result.output.as_deref().map(log_output).unwrap_or_default(),
                    "步骤结束"
                );
            }
            sink.emit(PlanRunEvent::StepFinished {
                index,
                record: record.clone(),
            });
            records.push(record);
        }

        // 只有模板里的每一步都 Done 才算成功（空模板不算成功）。
        // 不用可变标志：跳过分支不改标志会在「一上来就取消」时漏置。
        // 末尾可能追加的输出整理步**不参与**这里 —— 它失败只影响「有没有最终结果」，
        // 不改变任务本身的成败（所以必须在追加它之前先算完）。
        let success = !records.is_empty()
            && records
                .iter()
                .all(|record| record.status == StepStatus::Done);

        // ── 输出整理步 ──
        // 任务步全部成功后，按 `output_schema` 把交付步的输出整理成「最终结果」：
        // 契约缺失（用户跳过输出定义）则退化为交付步原文；任务未成功则不整理。
        let result = if success {
            self.resolve_result(
                template,
                params,
                &store,
                &mut records,
                sink,
                cancel.as_ref(),
            )
            .await
        } else {
            None
        };

        let report = PlanRunReport {
            success,
            total_duration_ms: started.elapsed().as_millis() as u64,
            prompt_tokens: records.iter().map(|record| record.prompt_tokens).sum(),
            completion_tokens: records.iter().map(|record| record.completion_tokens).sum(),
            tool_calls: records.iter().map(|record| record.tool_calls).sum(),
            steps: records,
            result,
        };
        if report.success {
            tracing::info!(
                steps = report.steps.len(),
                duration_ms = report.total_duration_ms,
                prompt_tokens = report.prompt_tokens,
                completion_tokens = report.completion_tokens,
                tool_calls = report.tool_calls,
                "灵活计划执行成功"
            );
        } else {
            let failed = report
                .steps
                .iter()
                .filter(|record| record.status == StepStatus::Failed)
                .count();
            let skipped = report
                .steps
                .iter()
                .filter(|record| record.status == StepStatus::Skipped)
                .count();
            tracing::warn!(
                failed,
                skipped,
                steps = report.steps.len(),
                duration_ms = report.total_duration_ms,
                prompt_tokens = report.prompt_tokens,
                completion_tokens = report.completion_tokens,
                tool_calls = report.tool_calls,
                "灵活计划执行结束：未全部成功"
            );
        }
        sink.emit(PlanRunEvent::RunFinished {
            report: report.clone(),
        });
        Ok(report)
    }

    /// 构建暴露给 LLM 的工具定义。
    ///
    /// 白名单过滤复用 `chat` 的 `select_tools_by_tokens` —— 规则单一来源，不做第二套。
    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let enabled = self.tools.get_enabled_tools_with_categories();
        let selected: Vec<Tool> = match &self.cfg.allowed_tools {
            None => enabled.into_iter().map(|(tool, _)| tool).collect(),
            Some(tokens) => crate::chat::tools::select_tools_by_tokens(enabled, tokens),
        };
        selected.into_iter().map(to_tool_definition).collect()
    }

    /// 按 `output_schema` 整理最终结果（必要时向 `records` 追加一步）。
    ///
    /// 返回 `None` 的四种情形：契约缺失且交付步无输出、没有可用的交付输出、
    /// 契约非法（同时补一条 `Failed` 的整理步）、整理步自身失败。
    async fn resolve_result(
        &self,
        template: &FlexiblePlanTemplate,
        params: &PlanRunParams,
        store: &HashMap<String, StoredOutput>,
        records: &mut Vec<StepRunRecord>,
        sink: &dyn PlanRunSink,
        cancel: Option<&watch::Receiver<bool>>,
    ) -> Option<String> {
        let Some(raw) = &template.output_schema else {
            // 没有契约（用户跳过输出定义步 / 选「定不了」）：结果退化为交付步原文
            let output = deliverable_output(template, store).map(|(_, output)| output.content.clone());
            tracing::info!(
                has_result = output.is_some(),
                "无输出契约：以交付步输出作为结果"
            );
            return output;
        };

        let schema = match OutputSchema::parse(raw) {
            Ok(schema) => schema,
            Err(reason) => {
                // 契约非法是**数据问题**：记一步 `Failed` 让原因可见，但不改变任务成败
                tracing::warn!(error = %reason, "输出契约非法，跳过结果整理");
                let index = records.len() + 1;
                sink.emit(PlanRunEvent::Failed {
                    index: Some(index),
                    error: reason.clone(),
                });
                records.push(resolve_failed_record(index, reason));
                return None;
            }
        };

        let Some((reference, stored)) = deliverable_output(template, store) else {
            tracing::warn!("有输出契约但没有可用的交付输出，跳过结果整理");
            return None;
        };

        let index = records.len() + 1;
        let intent = "按输出契约整理本次执行的最终结果".to_string();
        // 契约文本里的 `${name}` 换成本次参数的实际值（渲染失败就退回原文，不阻断）
        let contract = params
            .render(&prompt::build_output_contract_text(&schema))
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "输出契约渲染失败，用未渲染文本");
                prompt::build_output_contract_text(&schema)
            });

        // 交付步的产出必须给：未落盘时是全文，落盘后是「文件说明 + 预览」
        // （整理步因此需要 `builtin_read_file`，见下方 tools）。
        let preview_chars = self.cfg.spill_preview_chars;
        let mut prior = Vec::with_capacity(template.steps.len() + 1);
        prior.push((
            format!("{reference}（交付步的产出）"),
            render_prior_output(stored, preview_chars),
        ));
        // 各步产出一并给出，让整理步在末步信息不全时能回看 —— 超阈值的同样落盘，
        // 所以这里不会随步数线性膨胀（落盘后每条只剩「文件说明 + 预览」）。
        for step in template.steps.iter() {
            if step.result_reference == reference {
                continue;
            }
            if let Some(stored) = store.get(&step.result_reference) {
                prior.push((
                    format!("{} 的产出", step.result_reference),
                    render_prior_output(stored, preview_chars),
                ));
            }
        }

        let resolve_step = PlanStep {
            result_reference: RESOLVE_RESULT_REFERENCE.to_string(),
            intent: intent.clone(),
            expected_output: contract,
            dependencies: Vec::new(),
        };
        sink.emit(PlanRunEvent::StepStarted {
            index,
            intent: intent.clone(),
        });
        // 交付产出可能已落盘 —— 整理步必须能读回来（「不带工具」在落盘机制下不再成立）。
        // 只给这一个只读工具：整理仍是分析，不需要别的外部数据。
        let resolve_tools = tool_definitions_for_names(&self.tools, &["builtin_read_file"]);
        let outcome = run_output_resolve(
            StepInput {
                step: &resolve_step,
                intent: &intent,
                expected_output: resolve_step.expected_output.as_str(),
                prior: &prior,
                tools: &resolve_tools,
                index,
            },
            &self.ai,
            &self.tools,
            &self.cfg,
            sink,
            cancel,
        )
        .await;
        let result = outcome
            .output
            .clone()
            .filter(|_| outcome.record.status == StepStatus::Done);
        tracing::info!(
            step = index,
            status = ?outcome.record.status,
            result_chars = result.as_deref().map(str::len).unwrap_or(0),
            result = %result.as_deref().map(log_output).unwrap_or_default(),
            "输出整理步结束"
        );
        sink.emit(PlanRunEvent::StepFinished {
            index,
            record: outcome.record.clone(),
        });
        records.push(outcome.record);
        result
    }
}
/// 是否已收到取消信号。
fn is_cancelled(cancel: &Option<watch::Receiver<bool>>) -> bool {
    cancel
        .as_ref()
        .map(|receiver| *receiver.borrow())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests;
