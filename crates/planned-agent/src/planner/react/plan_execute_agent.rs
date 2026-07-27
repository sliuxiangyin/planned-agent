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
use tracing::{error, info};

use planned_agent_core::ai::AiClient;
use planned_agent_core::planner::coarse::{CoarseGrainedPlan, CoarsePlanner};
use planned_agent_core::planner::react::{ReActAgent, ReActAgentConfig};
use planned_agent_core::prompt::PromptManager;
use planned_agent_core::planner::react::StepResultStore;
use planned_agent_core::types::PlanContext;
use planned_agent_tool_manager::ToolRegistry;

use super::default_react_agent::DefaultReActAgent;
use crate::planner::coarse::LlmCoarsePlanner;

/// 单个步骤的执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_id: String,
    pub result_reference: String,
    pub intent: String,
    pub success: bool,
    pub output: Value,
    pub error: Option<String>,
    pub iterations: usize,
    pub duration_ms: u64,
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
                max_iterations: 5,
                step_timeout_ms: 30_000,
                enable_chain_of_thought: true,
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
    config: PlanAndExecuteConfig,
    /// 共享的步骤结果存储，与 fetch_step_result 工具共用
    store: StepResultStore,
}

impl<PM: PromptManager + 'static> PlanAndExecuteAgent<PM> {
    /// 创建新的 Plan-And-Execute Agent，内部维护步骤结果存储。
    pub fn new(
        ai_client: Arc<dyn AiClient>,
        prompt_manager: Arc<PM>,
        tool_registry: Arc<ToolRegistry>,
        config: PlanAndExecuteConfig,
    ) -> Self {
        let store = StepResultStore::new(std::sync::RwLock::new(HashMap::new()));
        Self {
            ai_client,
            prompt_manager,
            tool_registry,
            config,
            store,
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
            "[PlanAndExecute] 计划生成完成: {} 个步骤",
            coarse_plan.steps.len()
        );

        // ================================================================
        // 阶段 2：逐步执行
        // ================================================================
        info!("[PlanAndExecute] 阶段2: 逐步执行");

        let mut react_agent = DefaultReActAgent::new(
            self.ai_client.clone(),
            self.prompt_manager.clone(),
            self.tool_registry.clone(),
            self.config.react_config.clone(),
        );
        react_agent.set_store(self.store.clone());

        let mut step_results: HashMap<String, StepResult> = HashMap::new();

        for (i, step) in coarse_plan.steps.iter().enumerate() {
            info!(
                "[PlanAndExecute] 执行步骤 {}/{}: {} (ref={})",
                i + 1,
                coarse_plan.steps.len(),
                step.intent,
                step.result_reference
            );

            // ---- 更新共享 store：同步前序步骤结果 ----
            {
                let mut store = self.store.write().map_err(|e| {
                    anyhow::anyhow!("StepResultStore 写锁获取失败: {}", e)
                })?;
                for (ref_id, result) in &step_results {
                    store.insert(ref_id.clone(), result.output.clone());
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
                    let step_result = StepResult {
                        step_id: step.id.clone(),
                        result_reference: step.result_reference.clone(),
                        intent: step.intent.clone(),
                        success: result.success,
                        output: result.output.clone(),
                        error: result.error.clone(),
                        iterations: result.iterations,
                        duration_ms: result.total_duration_ms,
                    };

                    if result.success {
                        info!(
                            "[PlanAndExecute] 步骤 {} 成功 (ref={}, {}ms)",
                            step.id, step.result_reference, result.total_duration_ms
                        );
                    } else {
                        error!(
                            "[PlanAndExecute] 步骤 {} 失败 (ref={}): {:?}",
                            step.id, step.result_reference, result.error
                        );
                    }

                    step_results.insert(step.result_reference.clone(), step_result);
                }
                Err(e) => {
                    error!(
                        "[PlanAndExecute] 步骤 {} 执行异常 (ref={}): {}",
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
                            error: Some(e.to_string()),
                            iterations: 0,
                            duration_ms: 0,
                        },
                    );
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
