//! 总编排：按顺序跑每步，把前序输出递给后面的步，播事件，响应取消。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use planned_agent_core::ai::types::{FunctionDefinition, ToolDefinition, ToolType};
use planned_agent_core::ai::AiClient;
use planned_agent_core::mcp::types::Tool;
use planned_agent_tool_manager::ToolRegistry;
use tokio::sync::watch;

use super::event::{PlanRunEvent, PlanRunSink};
use super::output_schema::OutputSchema;
use super::params::{render_step_intent, PlanRunParams};
use super::prompt;
use super::report::{PlanRunReport, StepRunRecord, StepStatus};
use super::step::{run_output_resolve, run_step, StepInput};
use super::template::{FlexiblePlanTemplate, PlanStep};

/// 执行器配置。
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// 每步工具循环的轮数上限。
    pub max_rounds_per_step: usize,
    /// 采样温度（`None` 用 provider 默认）。
    pub temperature: Option<f32>,
    /// 最大生成 token（`None` 用 provider 默认）。
    pub max_tokens: Option<u32>,
    /// 工具白名单，语义与 `ChatConfig::allowed_tools` **完全一致**：
    /// `None` = 全部启用工具（含 Utility / SubAgent，不过滤）；
    /// `Some(tokens)` = 各 token 取并集（`"all"` / 分类名 / 精确工具名）。
    pub allowed_tools: Option<Vec<String>>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            max_rounds_per_step: 50,
            temperature: None,
            max_tokens: None,
            allowed_tools: None,
        }
    }
}

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
    pub async fn run(
        &self,
        template: &FlexiblePlanTemplate,
        params: &PlanRunParams,
        sink: &dyn PlanRunSink,
        cancel: Option<watch::Receiver<bool>>,
    ) -> Result<PlanRunReport> {
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
            task = %template.task.chars().take(80).collect::<String>(),
            "灵活计划开始执行"
        );

        let tools = self.tool_definitions();
        let mut store: HashMap<String, String> = HashMap::new();
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

            let intent = match render_step_intent(step, params) {
                Ok(intent) => intent,
                Err(err) => {
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

            let prior = collect_prior(step, &store);
            let result = run_step(
                StepInput {
                    step,
                    intent: &intent,
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

            if let Some(output) = &result.output {
                store.insert(step.result_reference.clone(), output.clone());
            }
            let record = result.record;
            if record.status == StepStatus::Failed {
                tracing::warn!(
                    step = index,
                    rounds = record.rounds,
                    tool_calls = record.tool_calls,
                    duration_ms = record.duration_ms,
                    error = record.error.as_deref().unwrap_or("未记录原因"),
                    "步骤失败"
                );
            } else {
                tracing::info!(
                    step = index,
                    status = ?record.status,
                    rounds = record.rounds,
                    tool_calls = record.tool_calls,
                    duration_ms = record.duration_ms,
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
        store: &HashMap<String, String>,
        records: &mut Vec<StepRunRecord>,
        sink: &dyn PlanRunSink,
        cancel: Option<&watch::Receiver<bool>>,
    ) -> Option<String> {
        let Some(raw) = &template.output_schema else {
            // 没有契约（用户跳过输出定义步 / 选「定不了」）：结果退化为交付步原文
            let output = deliverable_output(template, store).map(|(_, output)| output.to_string());
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

        let Some((reference, output)) = deliverable_output(template, store) else {
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

        // 交付步的完整输出必须给；再带上各步摘要，让整理步在末步信息不全时能回看。
        // 摘要不是装饰 —— 这是 `output_summary` 的第一个生产消费点。
        let mut prior = Vec::with_capacity(records.len() + 1);
        prior.push((
            format!("{reference}（交付步的完整输出）"),
            output.to_string(),
        ));
        for record in records.iter() {
            if let Some(summary) = &record.output_summary {
                prior.push((
                    format!("{} 的输出摘要", record.result_reference),
                    summary.clone(),
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
        // 不带工具：整理只做分析，不需要外部数据
        let outcome = run_output_resolve(
            StepInput {
                step: &resolve_step,
                intent: &intent,
                prior: &prior,
                tools: &[],
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

/// 输出整理步的结果引用标识（它不是模板步骤，标签固定）。
const RESOLVE_RESULT_REFERENCE: &str = "#RESULT";

/// 交付步的输出 —— 即最后一个「拿到了输出」的模板步骤。
///
/// 模板是粗粒度**线性骨架**，最后一步就是交付步（依赖图「出度 0」的判定在该形态下与
/// 「最后一步」等价，所以不引入依赖图分析）。失败 / 跳过的步骤不会进 `store`
/// （只有真拿到 output 才写入），所以这里天然只会取到有产出的那一步。
fn deliverable_output<'t, 's>(
    template: &'t FlexiblePlanTemplate,
    store: &'s HashMap<String, String>,
) -> Option<(&'t str, &'s str)> {
    template.steps.iter().rev().find_map(|step| {
        store
            .get(&step.result_reference)
            .map(|output| (step.result_reference.as_str(), output.as_str()))
    })
}

/// 契约非法时补一条 `Failed` 的整理步记录（让「为什么没有结果」在报告里可见）。
fn resolve_failed_record(index: usize, error: String) -> StepRunRecord {
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
        rounds: 0,
        call_usages: vec![],
        output_summary: None,
        output: None,
        output_truncated: false,
        error: Some(error),
    }
}

/// 收集某步依赖项的实际输出（按 `dependencies` 顺序）。
fn collect_prior(step: &PlanStep, store: &HashMap<String, String>) -> Vec<(String, String)> {
    step.dependencies
        .iter()
        .filter_map(|reference| {
            store
                .get(reference)
                .map(|output| (reference.clone(), output.clone()))
        })
        .collect()
}

/// 未执行步骤的占位记录（`Skipped` / 展开失败）。
fn placeholder_record(
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
        rounds: 0,
        call_usages: vec![],
        output_summary: None,
        output: None,
        output_truncated: false,
        error: error.map(str::to_string),
    }
}

/// `Tool` → LLM 侧的 `ToolDefinition`。
fn to_tool_definition(tool: Tool) -> ToolDefinition {
    ToolDefinition {
        r#type: ToolType::Function,
        function: FunctionDefinition {
            name: tool.name,
            description: Some(tool.description),
            parameters: Some(tool.input_schema),
            strict: None,
        },
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
mod tests {
    use super::*;
    use crate::flexible::testing::{text_response, FakeAiClient, RecordingSink};
    use crate::flexible::{PlanInput, PlanStep};

    /// 两步模板：第二步依赖第一步的 `#E1`，且 `intent` 含 `${path}` 占位符。
    fn two_step_template() -> FlexiblePlanTemplate {
        FlexiblePlanTemplate {
            output_schema: None,
            task: "维护文件".to_string(),
            inputs: vec![PlanInput {
                name: "path".to_string(),
                default: Some(serde_json::json!("a.txt")),
                description: None,
            }],
            steps: vec![
                PlanStep {
                    result_reference: "#E1".to_string(),
                    intent: "读取 ${path}".to_string(),
                    expected_output: "文件内容".to_string(),
                    dependencies: vec![],
                },
                PlanStep {
                    result_reference: "#E2".to_string(),
                    intent: "基于 #E1 追加一行".to_string(),
                    expected_output: "追加完成".to_string(),
                    dependencies: vec!["#E1".to_string()],
                },
            ],
        }
    }

    /// 有契约（`bool`）→ 追加一个整理步，结果取它的输出；契约渲染进请求，且**不带工具**。
    #[tokio::test]
    async fn output_contract_appends_resolve_step_and_returns_result() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
            text_response("文件末尾已新增一行（index=8）", 30, 3),
        ]);
        let template = FlexiblePlanTemplate {
            output_schema: Some(serde_json::json!({
                "kind": "bool",
                "goal": "向 ${path} 末尾追加一行",
                "success": "文件末尾新增一行即视为成功"
            })),
            ..two_step_template()
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success, "整理步不参与任务成败");
        assert_eq!(report.steps.len(), 3, "两个模板步 + 一个整理步");
        let resolve = &report.steps[2];
        assert_eq!(resolve.result_reference, "#RESULT");
        assert_eq!(resolve.index, 3);
        assert_eq!(resolve.status, StepStatus::Done);
        assert_eq!(
            report.result.as_deref(),
            Some("文件末尾已新增一行（index=8）"),
            "最终结果 = 整理步的输出"
        );

        // 第三次请求是整理步：system 换专用 prompt、契约渲染成实际值、交付步输出带上了
        let requests = ai.requests();
        assert_eq!(requests.len(), 3);
        let messages = serde_json::to_string(&requests[2].messages).unwrap();
        assert!(
            messages.contains("结果整理助手"),
            "应换整理专用 system prompt: {messages}"
        );
        assert!(
            messages.contains("文件末尾新增一行即视为成功"),
            "契约应进请求: {messages}"
        );
        assert!(
            messages.contains("第二步产出"),
            "交付步输出应进请求: {messages}"
        );
        assert!(
            messages.contains("向 a.txt 末尾追加一行"),
            "${{path}} 应被渲染: {messages}"
        );
        assert!(requests[2].tools.is_none(), "整理步不应带工具");

        // 整理步也要有自己的事件，UI 的 pipeline 才能显示它
        let events = sink.events();
        assert!(events.iter().any(|event| matches!(
            event,
            PlanRunEvent::StepStarted { index: 3, intent } if intent.contains("整理")
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, PlanRunEvent::StepFinished { index: 3, .. })));
    }

    /// 没有契约（用户跳过输出定义）→ 不追加整理步，结果退化为交付步原文，也不多花一次 LLM 调用。
    #[tokio::test]
    async fn missing_contract_falls_back_to_deliverable_output() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, &sink, None)
            .await
            .expect("run 不应失败");

        assert_eq!(report.steps.len(), 2, "无契约不应追加整理步");
        assert_eq!(report.result.as_deref(), Some("第二步产出"));
        assert_eq!(ai.requests().len(), 2, "无契约不应多一次 LLM 调用");
    }

    /// 契约非法是数据问题：记一条 `Failed` 的整理步让原因可见，但**不**把任务判失败。
    #[tokio::test]
    async fn invalid_contract_records_failed_resolve_step_without_failing_run() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = FlexiblePlanTemplate {
            output_schema: Some(serde_json::json!({ "kind": "success_only", "success": "s" })),
            ..two_step_template()
        };
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success, "契约非法不该把任务判成失败");
        assert_eq!(report.steps.len(), 3);
        let resolve = &report.steps[2];
        assert_eq!(resolve.status, StepStatus::Failed);
        assert!(resolve
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("success_only"));
        assert!(report.result.is_none());
        assert_eq!(ai.requests().len(), 2, "契约非法时不该再调一次 LLM");
    }

    fn executor(ai: Arc<FakeAiClient>) -> FlexibleExecutor {
        FlexibleExecutor::new(
            ai as Arc<dyn AiClient>,
            Arc::new(ToolRegistry::new()),
            ExecutorConfig::default(),
        )
    }

    #[tokio::test]
    async fn passes_prior_output_and_expands_params() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(report.success);
        assert_eq!(report.steps_done(), 2);
        assert_eq!(report.prompt_tokens, 30);
        assert_eq!(report.completion_tokens, 3);
        assert_eq!(report.tool_calls, 0);

        // 两个请求：第一个含展开后的参数（${path}），第二个含 #E1 的实际产出
        let requests = ai.requests();
        assert_eq!(requests.len(), 2);
        let first = serde_json::to_string(&requests[0].messages).unwrap();
        assert!(
            first.contains("a.txt"),
            "第一步 intent 应展开 ${{path}}: {first}"
        );
        let second = serde_json::to_string(&requests[1].messages).unwrap();
        assert!(
            second.contains("第一步产出"),
            "第二步应收到 #E1 的产出: {second}"
        );

        let events = sink.events();
        assert!(events
            .iter()
            .any(|event| matches!(event, PlanRunEvent::RunStarted { total_steps: 2 })));
        assert!(events
            .iter()
            .any(|event| matches!(event, PlanRunEvent::RunFinished { .. })));
    }

    #[tokio::test]
    async fn failure_marks_downstream_steps_skipped() {
        // 只给一个响应：第二步需要的响应缺失 → 第一步成功、但整体因缺响应而失败
        let ai = FakeAiClient::new(vec![text_response("只有第一步", 10, 1)]);
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();

        let report = executor(ai.clone())
            .run(&template, &params, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(!report.success, "第二步 LLM 失败应使整体失败");
        assert_eq!(report.steps[0].status, StepStatus::Done);
        assert_eq!(report.steps[1].status, StepStatus::Failed);
        assert_eq!(ai.requests().len(), 2);
    }

    #[tokio::test]
    async fn missing_param_marks_step_failed_and_skips_rest() {
        let template = two_step_template();
        // 空参数表：${path} 无值 → 第一步展开失败
        let params = PlanRunParams::new();
        let sink = RecordingSink::default();
        let ai = FakeAiClient::new(vec![text_response("不该被调用", 1, 1)]);

        let report = executor(ai.clone())
            .run(&template, &params, &sink, None)
            .await
            .expect("run 不应失败");

        assert!(!report.success);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert_eq!(report.steps[1].status, StepStatus::Skipped);
        assert!(ai.requests().is_empty(), "展开失败时不应发起 LLM 请求");
    }

    #[tokio::test]
    async fn cancelled_before_start_skips_everything() {
        let template = two_step_template();
        let params = PlanRunParams::from_template(&template);
        let sink = RecordingSink::default();
        let ai = FakeAiClient::new(vec![text_response("不该被调用", 1, 1)]);

        let (tx, rx) = watch::channel(true);
        drop(tx);

        let report = executor(ai.clone())
            .run(&template, &params, &sink, Some(rx))
            .await
            .expect("run 不应失败");

        assert!(!report.success);
        assert!(report
            .steps
            .iter()
            .all(|step| step.status == StepStatus::Skipped));
        assert!(ai.requests().is_empty());
    }
}
