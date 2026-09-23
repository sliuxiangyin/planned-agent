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
use super::params::{render_step_intent, PlanRunParams};
use super::report::{PlanRunReport, StepRunRecord, StepStatus};
use super::step::{run_step, StepInput};
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
            max_rounds_per_step: 10,
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
                records.push(placeholder_record(index, step, StepStatus::Skipped, Some(reason)));
                continue;
            }

            let intent = match render_step_intent(step, params) {
                Ok(intent) => intent,
                Err(err) => {
                    let message = err.to_string();
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
            sink.emit(PlanRunEvent::StepFinished {
                index,
                record: result.record.clone(),
            });
            records.push(result.record);
        }

        // 只有每一步都 Done 才算成功（空模板不算成功）。
        // 不用可变标志：跳过分支不改标志会在「一上来就取消」时漏置。
        let success = !records.is_empty()
            && records
                .iter()
                .all(|record| record.status == StepStatus::Done);

        let report = PlanRunReport {
            success,
            total_duration_ms: started.elapsed().as_millis() as u64,
            prompt_tokens: records.iter().map(|record| record.prompt_tokens).sum(),
            completion_tokens: records.iter().map(|record| record.completion_tokens).sum(),
            tool_calls: records.iter().map(|record| record.tool_calls).sum(),
            steps: records,
        };
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
        assert!(first.contains("a.txt"), "第一步 intent 应展开 ${{path}}: {first}");
        let second = serde_json::to_string(&requests[1].messages).unwrap();
        assert!(second.contains("第一步产出"), "第二步应收到 #E1 的产出: {second}");

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
        assert!(report.steps.iter().all(|step| step.status == StepStatus::Skipped));
        assert!(ai.requests().is_empty());
    }
}
