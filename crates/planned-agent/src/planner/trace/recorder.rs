//! 轨迹记录器（Phase 1）
//!
//! 职责：收集步骤成功执行数据 → LLM 泛化 → 校验 → JSON 文件存储。
//! Prompt 模板由 prompt-manager 管理（`planning/trace_generalize`），
//! prompt-manager 不可用时使用内置兜底模板。

use anyhow::Result;
use chrono::Local;
use planned_agent_ai_manager::AiManager;
use planned_agent_core::ai::AiClient;
use planned_agent_core::planner::react::ReActStep;
use planned_agent_core::planner::trace::{ExecutionTrace, GeneralizedAction};
use planned_agent_core::prompt::{PromptContext, PromptManager};
use planned_agent_core::types::{ChatCompletionRequest, Message, MessageContent, MessageRole};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn};

/// 轨迹记录器配置
#[derive(Debug, Clone)]
pub struct TraceRecorderConfig {
    /// 是否启用
    pub enabled: bool,
    /// 轨迹存储目录
    pub storage_dir: PathBuf,
    /// 入库质量门槛：迭代次数超过此值不入库
    pub max_iterations_for_record: usize,
    /// 是否使用 LLM 泛化（false = 仅规则泛化）
    pub use_llm_generalization: bool,
    /// 泛化使用的模型名称（None = 用 AiManager.default()）
    pub trace_model: Option<String>,
}

impl Default for TraceRecorderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            storage_dir: PathBuf::from("./traces"),
            max_iterations_for_record: 5,
            use_llm_generalization: true,
            trace_model: None,
        }
    }
}

/// 轨迹记录器
///
/// 每个步骤成功后，调用 `record_successful_step` 完成：
/// 1. 从 history 提取工具调用序列 + observation 摘要
/// 2. LLM 泛化（或规则兜底）
/// 3. 序列化为 JSON 写入 traces/ 目录
pub struct TraceRecorder<PM: PromptManager> {
    config: TraceRecorderConfig,
    /// AI 管理器（按 trace_model 或 default 获取泛化客户端）
    ai_manager: Option<Arc<AiManager>>,
    /// Prompt 管理器（用于加载泛化模板）
    prompt_manager: Option<Arc<PM>>,
    /// 全局序号生成器
    seq: AtomicU64,
}

/// 从 ReActStep 提取的单步上下文（供泛化 LLM 推断数据流转）
struct StepContext {
    tool_name: String,
    params: Value,
    observation_summary: String,
}

impl<PM: PromptManager + 'static> TraceRecorder<PM> {
    /// 创建新的轨迹记录器
    ///
    /// - `ai_manager`: AI 管理器，用于按模型名获取泛化客户端
    /// - `prompt_manager`: 用于加载泛化 Prompt 模板，None 则使用内置兜底
    pub fn new(
        config: TraceRecorderConfig,
        ai_manager: Option<Arc<AiManager>>,
        prompt_manager: Option<Arc<PM>>,
    ) -> Self {
        if config.enabled {
            if let Err(e) = std::fs::create_dir_all(&config.storage_dir) {
                warn!("无法创建轨迹存储目录 {:?}: {}", config.storage_dir, e);
            }
        }

        Self {
            config,
            ai_manager,
            prompt_manager,
            seq: AtomicU64::new(1),
        }
    }

    /// 记录一个成功执行步骤
    pub async fn record_successful_step(
        &self,
        intent: &str,
        upstream_intent: Option<&str>,
        history: &[ReActStep],
        iterations: usize,
        duration_ms: u64,
    ) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        if iterations > self.config.max_iterations_for_record {
            info!(
                "[TraceRecorder] 跳过记录：迭代次数 {} 超过门槛 {}",
                iterations, self.config.max_iterations_for_record
            );
            return Ok(());
        }

        let contexts = Self::extract_step_contexts(history);
        if contexts.is_empty() {
            info!("[TraceRecorder] 跳过记录：无工具调用");
            return Ok(());
        }

        let (generalized_intent, actions) = if self.config.use_llm_generalization
            && self.ai_manager.is_some()
        {
            match self.llm_generalize(intent, &contexts).await {
                Ok((gi, acts)) => {
                    info!("[TraceRecorder] LLM 泛化成功");
                    (gi, acts)
                }
                Err(e) => {
                    warn!("[TraceRecorder] LLM 泛化失败: {}，退化为规则泛化", e);
                    self.rule_generalize(intent, &contexts)
                }
            }
        } else {
            self.rule_generalize(intent, &contexts)
        };

        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let today = Local::now().format("%Y-%m-%d").to_string();
        let trace_id = format!("{}-{:03}", today, seq);

        let trace = ExecutionTrace {
            id: trace_id.clone(),
            original_intent: intent.to_string(),
            generalized_intent,
            upstream_intent: upstream_intent.map(|s| s.to_string()),
            actions,
            total_iterations: iterations,
            total_duration_ms: duration_ms,
            recorded_at: Local::now().to_rfc3339(),
        };

        let filename = format!("{}.json", trace_id);
        let filepath = self.config.storage_dir.join(&filename);
        let json = serde_json::to_string_pretty(&trace)?;
        tokio::fs::write(&filepath, json).await?;

        info!("[TraceRecorder] 轨迹已保存: {}", filepath.display());
        Ok(())
    }

    /// 从 chat_with_callback 产生的消息历史中记录执行轨迹（灵活模式专用）。
    ///
    /// 与 `record_successful_step` 不同：输入是原始 `Vec<Message>` 而非
    /// `ReActStep[]`。内部提取 tool 调用序列后走相同的泛化→存储流程。
    pub async fn record_from_chat_history(
        &self,
        plan_intent: &str,
        messages: &[Message],
        duration_ms: u64,
    ) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        // 从 messages 提取 ReActStep 序列
        let steps = crate::planner::react::flexible_plan_agent::extract_react_steps_from_messages(messages);

        if steps.is_empty() {
            info!("[TraceRecorder] 跳过记录：消息中无工具调用");
            return Ok(());
        }

        let iterations = steps.len();
        if iterations > self.config.max_iterations_for_record {
            info!(
                "[TraceRecorder] 跳过记录：工具调用次数 {} 超过门槛 {}",
                iterations, self.config.max_iterations_for_record
            );
            return Ok(());
        }

        let contexts = Self::extract_step_contexts(&steps);
        if contexts.is_empty() {
            return Ok(());
        }

        let (generalized_intent, actions) = if self.config.use_llm_generalization
            && self.ai_manager.is_some()
        {
            match self.llm_generalize(plan_intent, &contexts).await {
                Ok((gi, acts)) => {
                    info!("[TraceRecorder] LLM 泛化成功（灵活模式）");
                    (gi, acts)
                }
                Err(e) => {
                    warn!("[TraceRecorder] LLM 泛化失败: {}，退化为规则泛化", e);
                    self.rule_generalize(plan_intent, &contexts)
                }
            }
        } else {
            self.rule_generalize(plan_intent, &contexts)
        };

        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let today = Local::now().format("%Y-%m-%d").to_string();
        let trace_id = format!("{}-{:03}", today, seq);

        let trace = ExecutionTrace {
            id: trace_id.clone(),
            original_intent: plan_intent.to_string(),
            generalized_intent,
            upstream_intent: None,
            actions,
            total_iterations: iterations,
            total_duration_ms: duration_ms,
            recorded_at: Local::now().to_rfc3339(),
        };

        let filename = format!("{}.json", trace_id);
        let filepath = self.config.storage_dir.join(&filename);
        let json = serde_json::to_string_pretty(&trace)?;
        tokio::fs::write(&filepath, json).await?;

        info!("[TraceRecorder] 灵活模式轨迹已保存: {}", filepath.display());
        Ok(())
    }

    // ── 内部方法 ────────────────────────────────────────

    fn extract_step_contexts(history: &[ReActStep]) -> Vec<StepContext> {
        history
            .iter()
            .map(|step| {
                let summary = Self::summarize_observation(&step.observation.output);
                StepContext {
                    tool_name: step.action.tool_name.clone(),
                    params: step.action.parameters.clone(),
                    observation_summary: summary,
                }
            })
            .collect()
    }

    fn summarize_observation(output: &Value) -> String {
        let raw = match output {
            Value::String(s) => s.clone(),
            _ => serde_json::to_string(output).unwrap_or_default(),
        };
        let limit = 200;
        if raw.chars().count() > limit {
            let truncated: String = raw.chars().take(limit).collect();
            format!("{}...(已截断，全长 {} 字符)", truncated, raw.chars().count())
        } else {
            raw
        }
    }

    // ── 泛化 Prompt 构建 ──────────────────────────────

    /// 获取泛化 Prompt 文本（优先 prompt-manager，不可用时内置兜底）
    async fn get_generalization_prompt(
        &self,
        intent: &str,
        contexts: &[StepContext],
    ) -> String {
        let tool_calls_text = Self::format_tool_calls(contexts);

        // 尝试用 prompt-manager 渲染
        if let Some(ref pm) = self.prompt_manager {
            let ctx = PromptContext::new()
                .with_variable("intent", json!(intent))
                .with_variable("tool_calls", json!(tool_calls_text));

            match pm.render("planning/trace_generalize", &ctx).await {
                Ok(rendered) => return rendered,
                Err(e) => {
                    warn!("[TraceRecorder] prompt-manager 渲染失败: {}，使用内置模板", e);
                }
            }
        }

        // 内置兜底模板
        Self::builtin_generalization_prompt(intent, &tool_calls_text)
    }

    /// 格式化工具调用序列为文本
    fn format_tool_calls(contexts: &[StepContext]) -> String {
        contexts
            .iter()
            .enumerate()
            .map(|(i, ctx)| {
                format!(
                    "{}. {}({})\n   输出摘要: {}",
                    i + 1,
                    ctx.tool_name,
                    serde_json::to_string(&ctx.params).unwrap_or_default(),
                    ctx.observation_summary,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 内置兜底模板（prompt-manager 不可用时使用）
    fn builtin_generalization_prompt(intent: &str, tool_calls_text: &str) -> String {
        format!(
            r#"将以下操作流程中的具体实例值（人名、地名、关键词、URL、数字、文件名）替换为 {{{{变量名}}}} 占位符，生成可复用的模板。

规则：
1. 只替换具体实例值，结构字段（selector、API 路径、工具名）保持不变
2. 工具调用顺序不变
3. 变量名用中文，描述其含义（如 {{{{关键词}}}}、{{{{文件路径}}}}）
4. 根据每个步骤的"输出摘要"，推断数据如何在步骤间流转，填入 reasoning_hint
5. 只输出 JSON，不要其他文字

输入意图：{intent}

工具调用序列（含输出摘要）：
{tool_calls_text}

输出 JSON 格式：
{{{{
  "generalized_intent": "泛化后的意图",
  "actions": [
    {{{{
      "tool_name": "工具名",
      "params": {{"key": "{{{{变量}}}}"}},
      "description": "步骤说明",
      "reasoning_hint": "从前一步的输出中提取了什么数据，为什么选择这些参数"
    }}}}
  ]
}}}}"#,
            intent = intent,
        )
    }

    // ── LLM 调用和解析 ───────────────────────────────

    async fn llm_generalize(
        &self,
        intent: &str,
        contexts: &[StepContext],
    ) -> Result<(String, Vec<GeneralizedAction>)> {
        let manager = self
            .ai_manager
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("无可用 AI 管理器"))?;

        // 按 trace_model 选模型，未指定则用默认
        let client: Arc<dyn AiClient> = if let Some(ref model) = self.config.trace_model {
            manager.get(model).map_err(|e| {
                warn!("[TraceRecorder] 指定模型 '{}' 不可用: {}，回退到默认", model, e);
                e
            }).or_else(|_| manager.default()).map_err(|_| {
                anyhow::anyhow!("无可用的 AI 客户端")
            })?
        } else {
            manager.default()?
        };

        let prompt = self.get_generalization_prompt(intent, contexts).await;

        let request = ChatCompletionRequest {
            model: client.model_name().to_string(),
            messages: vec![Message {
                role: MessageRole::User,
                content: Some(MessageContent::Text { text: prompt }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            }],
            tools: None,
            temperature: Some(0.0),
            max_tokens: Some(2000),
            stream: false,
            extra: HashMap::new(),
        };

        let response = client.chat_completion(request).await?;

        let text = response
            .choices
            .first()
            .and_then(|c| match &c.message.content {
                Some(MessageContent::Text { text }) => Some(text.as_str()),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("LLM 泛化返回空内容"))?;

        Self::parse_generalization_result(text, contexts)
    }

    fn parse_generalization_result(
        text: &str,
        contexts: &[StepContext],
    ) -> Result<(String, Vec<GeneralizedAction>)> {
        let json_str = text
            .trim()
            .strip_prefix("```json")
            .or_else(|| text.trim().strip_prefix("```"))
            .map(|s| s.strip_suffix("```").unwrap_or(s))
            .unwrap_or(text)
            .trim();

        let parsed: Value = serde_json::from_str(json_str)?;

        let generalized_intent = parsed["generalized_intent"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("JSON 缺少 generalized_intent 字段"))?
            .to_string();

        if !generalized_intent.contains("{{") {
            return Err(anyhow::anyhow!(
                "泛化结果不包含 {{变量}} 占位符，视为失败"
            ));
        }

        let actions: Vec<GeneralizedAction> = parsed["actions"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("JSON 缺少 actions 数组"))?
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let original_params = contexts
                    .get(i)
                    .map(|c| c.params.clone())
                    .unwrap_or(Value::Null);

                GeneralizedAction {
                    tool_name: a["tool_name"].as_str().unwrap_or("unknown").to_string(),
                    params: a["params"].clone(),
                    original_params,
                    description: a["description"].as_str().unwrap_or("").to_string(),
                    reasoning_hint: a["reasoning_hint"].as_str().unwrap_or("").to_string(),
                }
            })
            .collect();

        Ok((generalized_intent, actions))
    }

    // ── 规则泛化兜底 ─────────────────────────────────

    fn rule_generalize(
        &self,
        intent: &str,
        contexts: &[StepContext],
    ) -> (String, Vec<GeneralizedAction>) {
        let generalized_intent = intent.to_string();

        let actions: Vec<GeneralizedAction> = contexts
            .iter()
            .map(|ctx| {
                let original = ctx.params.clone();
                let generalized = Self::rule_generalize_params(&ctx.params);
                let desc = "执行操作".to_string();
                let hint = {
                    let limit = 100;
                    if ctx.observation_summary.chars().count() > limit {
                        let truncated: String = ctx.observation_summary.chars().take(limit).collect();
                        format!("输出: {}...", truncated)
                    } else {
                        format!("输出: {}", ctx.observation_summary)
                    }
                };

                GeneralizedAction {
                    tool_name: ctx.tool_name.clone(),
                    params: generalized,
                    original_params: original,
                    description: desc,
                    reasoning_hint: hint,
                }
            })
            .collect();

        (generalized_intent, actions)
    }

    fn rule_generalize_params(params: &Value) -> Value {
        match params {
            Value::Object(map) => {
                let mut new_map = serde_json::Map::new();
                for (k, v) in map {
                    let new_v = match v {
                        Value::String(s)
                            if !k.contains("selector")
                                && !k.contains("css")
                                && !k.contains("xpath")
                                && !k.contains("path")
                                && !k.contains("url")
                                && !k.contains("expr") =>
                        {
                            if s.parse::<f64>().is_ok() {
                                json!("{{数字}}")
                            } else if s.starts_with("http://") || s.starts_with("https://") {
                                json!("{{URL}}")
                            } else if s.starts_with('#') || s.starts_with('.') {
                                v.clone()
                            } else {
                                json!("{{值}}")
                            }
                        }
                        Value::Number(_) => json!("{{数字}}"),
                        _ => v.clone(),
                    };
                    new_map.insert(k.clone(), new_v);
                }
                Value::Object(new_map)
            }
            _ => params.clone(),
        }
    }
}
