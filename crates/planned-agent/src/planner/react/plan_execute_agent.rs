//! Plan-And-Execute Agent
//!
//! 封装完整的"粗粒度计划生成 → ReAct 逐步执行"流水线。
//! 通过 `fetch_step_result` 动态工具实现步骤间数据传递：
//! - 每个步骤完成后按 result_reference 存入共享 store
//! - 后续步骤的 AI 按需调用 `fetch_step_result("#E1")` 获取前序数据
//! - 第一步无前序结果时工具 disabled，后续步骤动态 enabled
//!
//! 调用方式：
//! ```ignore
//! let mut pae = PlanAndExecuteAgent::new(ai_client, prompt_manager, tool_registry, config);
//! let result = pae.execute(user_input, &plan_context).await?;
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{error, info, warn};

use planned_agent_core::ai::AiClient;
use planned_agent_core::planner::coarse::{CoarseGrainedPlan, CoarsePlanner};
use planned_agent_core::planner::react::{ReActAgent, ReActAgentConfig, ReActStep};
use planned_agent_core::prompt::PromptManager;
use planned_agent_core::types::PlanContext;
use planned_agent_tool_manager::ToolRegistry;

use super::default_react_agent::DefaultReActAgent;
use super::chunk::executor_context::ExecutorContext;
use super::step_store::StepStore;
use crate::planner::coarse::LlmCoarsePlanner;
use crate::planner::trace::TraceRecorder;

/// 单个步骤的执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_id: String,
    pub result_reference: String,
    pub intent: String,
    pub success: bool,
    pub output: Value,
    /// observe 阶段的执行摘要
    pub observe_summary: Option<String>,
    pub error: Option<String>,
    pub iterations: usize,
    pub duration_ms: u64,
    /// 每轮完整执行历史（think+act+observe）
    pub history: Vec<ReActStep>,
}

/// Plan-And-Execute 整体结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanAndExecuteResult {
    pub coarse_plan: CoarseGrainedPlan,
    pub step_results: HashMap<String, StepResult>,
    pub success: bool,
    pub total_duration_ms: u64,
}

/// Plan-And-Execute Agent 配置
#[derive(Debug, Clone)]
pub struct PlanAndExecuteConfig {
    pub react_config: ReActAgentConfig,
}

impl Default for PlanAndExecuteConfig {
    fn default() -> Self {
        Self {
            react_config: ReActAgentConfig {
                max_iterations: 15,
                step_timeout_ms: 60_000,
                max_retries: 3,
                retry_delay_ms: 1000,
            },
        }
    }
}

/// Plan-And-Execute Agent
///
/// 封装完整的流水线，通过共享 `StepResultStore` + 动态工具 `fetch_step_result`
/// 实现步骤间的按需数据传递。
pub struct PlanAndExecuteAgent<PM: PromptManager> {
    ai_client: Arc<dyn AiClient>,
    prompt_manager: Arc<PM>,
    tool_registry: Arc<ToolRegistry>,
    exec_ctx: Arc<ExecutorContext>,
    config: PlanAndExecuteConfig,
    /// 共享的步骤结果存储，与 fetch_step_result 工具共用
    store: StepStore,
    /// 轨迹记录器（步骤成功后 LLM 泛化 + JSON 存储）
    trace_recorder: TraceRecorder<PM>,
}

impl<PM: PromptManager + 'static> PlanAndExecuteAgent<PM> {
    /// 创建新的 Plan-And-Execute Agent，内部维护步骤结果存储。
    pub fn new(
        ai_client: Arc<dyn AiClient>,
        prompt_manager: Arc<PM>,
        tool_registry: Arc<ToolRegistry>,
        exec_ctx: Arc<ExecutorContext>,
        config: PlanAndExecuteConfig,
        trace_recorder: TraceRecorder<PM>,
    ) -> Self {
        let store = StepStore::new();
        Self {
            ai_client,
            prompt_manager,
            tool_registry,
            exec_ctx,
            config,
            store,
            trace_recorder,
        }
    }

    /// 执行 Plan-And-Execute 流水线
    pub async fn execute(
        &mut self,
        user_input: &str,
        plan_context: &PlanContext,
    ) -> Result<PlanAndExecuteResult> {
        let total_start = std::time::Instant::now();

        // ================================================================
        // 阶段 1：生成粗粒度计划
        // ================================================================
        info!("[PlanAndExecute] 阶段1: 生成粗粒度计划");

        let coarse_planner = LlmCoarsePlanner::new(
            self.ai_client.clone(),
            self.prompt_manager.clone(),
        );

        let coarse_plan = coarse_planner
            .generate_coarse_plan(user_input, plan_context)
            .await?;

        info!(
            "[PlanAndExecute] 计划生成结果: {:?} ",
            coarse_plan
        );

        // ================================================================
        // 阶段 2：逐步执行
        // ================================================================
        info!("[PlanAndExecute] 阶段2: 逐步执行");

        let mut react_agent = DefaultReActAgent::new(
            self.ai_client.clone(),
            self.prompt_manager.clone(),
            self.tool_registry.clone(),
            self.exec_ctx.clone(),
            self.config.react_config.clone(),
        );
        react_agent.set_store(self.store.clone());

        let mut step_results: HashMap<String, StepResult> = HashMap::new();

        for (i, step) in coarse_plan.steps.iter().enumerate() {
            info!(
                "[PlanAndExecute] 执行步骤 {}/{}: {} (ref={})",
                i + 1,
                step.id,
                step.intent,
                step.result_reference
            );

            // ---- 更新共享 store：同步前序步骤结果 ----
            {
                for (ref_id, result) in &step_results {
                    if result.success {
                        if let Err(e) = self.store.insert(
                            ref_id,
                            result.output.clone(),
                            result.observe_summary.clone(),
                        ) {
                            anyhow::bail!("StepStore 写入失败: {}", e);
                        }
                    } else {
                        // 失败步骤存错误信息，避免后续步骤拿到 null 误判为有数据
                        if let Err(e) = self.store.insert(
                            ref_id,
                            serde_json::json!({
                                "error": true,
                                "message": result.error.clone().unwrap_or_else(|| "步骤执行失败".to_string()),
                                "step_id": result.step_id,
                                "hint": "此步骤执行失败，无可用数据，请基于前序可用的步骤继续"
                            }),
                            None,  // 失败步骤无摘要
                        ) {
                            anyhow::bail!("StepStore 写入失败: {}", e);
                        }
                    }
                }
            }
            if !step_results.is_empty() {
                info!("[PlanAndExecute] store 已更新，可用引用: {:?}",
                    step_results.keys().collect::<Vec<_>>());
            }

            // 构建当前步骤的上下文
            let mut step_context = plan_context.clone();
            step_context
                .metadata
                .insert("user_goal".to_string(), serde_json::json!(user_input));

            // 注入后续步骤信息
            let remaining_steps: Vec<_> = coarse_plan.steps.iter().skip(i + 1).collect();
            step_context.metadata.insert(
                "remaining_steps".to_string(),
                serde_json::json!(remaining_steps),
            );

            // 执行当前步骤
            match react_agent.execute_coarse_step(step, &step_context).await {
                Ok(result) => {
                    if result.success {
                        info!(
                            "[PlanAndExecute] 步骤 {} 成功 (ref={}, {}ms)",
                            step.id, step.result_reference, result.total_duration_ms
                        );

                        // ✅ 记录成功轨迹（LLM 泛化 + JSON 存储）
                        let prev_intent = if i > 0 {
                            Some(coarse_plan.steps[i - 1].intent.as_str())
                        } else {
                            None
                        };
                        if let Err(e) = self.trace_recorder.record_successful_step(
                            &step.intent,
                            prev_intent,
                            &result.history,
                            result.iterations,
                            result.total_duration_ms,
                        ).await {
                            warn!("[PlanAndExecute] 轨迹记录失败: {}", e);
                        }
                    } else {
                        error!(
                            "[PlanAndExecute] 步骤 {} 失败 (ref={}): {:?}，终止流水线",
                            step.id, step.result_reference, result.error
                        );
                    }

                    let step_result = StepResult {
                        step_id: step.id.clone(),
                        result_reference: step.result_reference.clone(),
                        intent: step.intent.clone(),
                        success: result.success,
                        output: result.output.clone(),
                        observe_summary: result.observe_summary.clone(),
                        error: result.error.clone(),
                        iterations: result.iterations,
                        duration_ms: result.total_duration_ms,
                        history: result.history,
                    };

                    step_results.insert(step.result_reference.clone(), step_result);
                    if !result.success {
                        break;
                    }
                }
                Err(e) => {
                    error!(
                        "[PlanAndExecute] 步骤 {} 执行异常 (ref={}): {}，终止流水线",
                        step.id, step.result_reference, e
                    );
                    step_results.insert(
                        step.result_reference.clone(),
                        StepResult {
                            step_id: step.id.clone(),
                            result_reference: step.result_reference.clone(),
                            intent: step.intent.clone(),
                            success: false,
                            output: Value::Null,
                            observe_summary: None,
                            error: Some(e.to_string()),
                            iterations: 0,
                            duration_ms: 0,
                            history: Vec::new(),
                        },
                    );
                    break;
                }
            }
        }

        let total_duration_ms = total_start.elapsed().as_millis() as u64;
        let overall_success = step_results.values().all(|r| r.success);

        info!(
            "[PlanAndExecute] 流水线完成: success={}, {}ms",
            overall_success, total_duration_ms
        );

        Ok(PlanAndExecuteResult {
            coarse_plan,
            step_results,
            success: overall_success,
            total_duration_ms,
        })
    }
}
