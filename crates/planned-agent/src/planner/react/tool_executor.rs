//! 工具执行调度器
//!
//! 将 ReAct Agent 中所有工具执行逻辑集中管理：
//! - 工具解析与定义构建
//! - 特殊工具处理器（fetch_step_result / chunk_read / chunk_search / chunk_summary / ai_process）
//! - 通用工具调用 + 分片检测 + 日志
//! - LLM 调用辅助函数

use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::info;

use planned_agent_core::ai::AiClient;
use planned_agent_core::planner::coarse::CoarseGrainedStep;
use planned_agent_core::planner::react::Observation;
use planned_agent_core::tool_registry::ToolCategory;
use planned_agent_core::ai::types::{ChatCompletionRequest, FunctionDefinition, Message, MessageContent, MessageRole, ToolDefinition, ToolType};
use planned_agent_core::mcp::types::Tool;
use planned_agent_tool_manager::ToolRegistry;

use super::agent_context::AgentContext;
use super::chunk::ChunkStore;
use super::step_store::StepStore;

// ═══════════════════════════════════════════════════════════
// 工具解析
// ═══════════════════════════════════════════════════════════

/// 根据 categories 解析工具列表（空或 None 时兜底返回全部）
pub(crate) fn resolve_tools(
    tool_registry: &Arc<ToolRegistry>,
    categories: &[ToolCategory],
) -> Vec<Tool> {
    if categories.is_empty() {
        tool_registry.get_all_tools()
    } else {
        tool_registry.get_tools_by_categories(categories)
    }
}

/// 获取可用工具列表描述（文本形式，注入 System prompt）
pub(crate) fn get_tools_description(
    tool_registry: &Arc<ToolRegistry>,
    categories: &[ToolCategory],
) -> String {
    let tools = resolve_tools(tool_registry, categories);
    let mut desc = String::new();

    for tool in &tools {
        desc.push_str(&format!(
            "- {}: {}\n  Schema：{}\n",
            tool.name,
            tool.description,
            serde_json::to_string(&tool.input_schema).unwrap_or_default()
        ));
    }

    desc
}

/// 根据 step 构建 ToolDefinition 列表（含类别工具）
///
/// `override_categories` 允许调用方替换 step 的静态分类（如运行时根据依赖产出动态补充）。
///
/// 返回 None 表示无可用工具。
pub(crate) fn build_tool_definitions(
    tool_registry: &Arc<ToolRegistry>,
    step: &CoarseGrainedStep,
    override_categories: Option<&[ToolCategory]>,
) -> Option<Vec<ToolDefinition>> {
    let categories = override_categories
        .unwrap_or(step.recommended_tool_categories.as_deref().unwrap_or(&[]));
    let tools = resolve_tools(tool_registry, categories);

    if tools.is_empty() {
        return None;
    }
    Some(
        tools
            .iter()
            .map(|t| ToolDefinition {
                r#type: ToolType::Function,
                function: FunctionDefinition {
                    name: t.name.clone(),
                    description: Some(t.description.clone()),
                    parameters: Some(t.input_schema.clone()),
                    strict: None,
                },
            })
            .collect(),
    )
}

// ═══════════════════════════════════════════════════════════
// LLM 调用辅助
// ═══════════════════════════════════════════════════════════

/// 从 Message 中提取 content 文本
pub(crate) fn extract_text_content(message: &Message) -> Result<String> {
    match &message.content {
        Some(MessageContent::Text { text }) => Ok(text.clone()),
        _ => Err(anyhow!("No text content in message")),
    }
}

/// 调用 LLM（带完整消息列表，可选 tools）
pub(crate) async fn call_llm_with_messages(
    ai_client: &Arc<dyn AiClient>,
    messages: &[Message],
    tools: Option<Vec<ToolDefinition>>,
) -> Result<Message> {
    let request = ChatCompletionRequest {
        model: ai_client.model_name().to_string(),
        messages: messages.to_vec(),
        tools,
        temperature: Some(0.3),
        max_tokens: Some(8000),
        stream: false,
        extra: Default::default(),
    };

    let response = ai_client.chat_completion(request).await?;

    if let Some(choice) = response.choices.into_iter().next() {
        Ok(choice.message)
    } else {
        Err(anyhow!("No choices in response"))
    }
}

/// 调用 LLM（单 prompt，构建临时消息列表）
pub(crate) async fn call_llm(ai_client: &Arc<dyn AiClient>, prompt: &str) -> Result<String> {
    let messages = vec![Message {
        role: MessageRole::User,
        content: Some(MessageContent::Text {
            text: prompt.to_string(),
        }),
        tool_calls: None,
        tool_call_id: None,
        name: None,
        reasoning_content: None,
        ..Default::default()
    }];
    let response = call_llm_with_messages(ai_client, &messages, None).await?;
    extract_text_content(&response)
}

// ═══════════════════════════════════════════════════════════
// 特殊工具处理器
// ═══════════════════════════════════════════════════════════

/// 处理 ai_process：展开引用后走独立 AI 子流程
pub(crate) async fn handle_ai_process(
    ai_client: &Arc<dyn AiClient>,
    parameters: &Value,
    start_time: Instant,
) -> Result<Observation> {
    let data = parameters.get("data").cloned().unwrap_or(Value::Null);
    let instruction = parameters
        .get("instruction")
        .and_then(|v| v.as_str())
        .unwrap_or("处理数据");

    // 构建prompt
    let data_str = serde_json::to_string_pretty(&data)
        .unwrap_or_else(|_| serde_json::to_string(&data).unwrap_or_default());

    let prompt = format!(
        "请根据以下指令处理数据：\n\n指令：{}\n\n数据：\n{}\n\n请直接返回处理结果，不要包含其他说明。",
        instruction, data_str
    );

    // 重试机制：最多重试3次
    let max_retries = 3;
    let mut last_error = None;

    for _attempt in 0..max_retries {
        let result = call_llm(ai_client, &prompt).await;

        match result {
            Ok(response) => {
                let output = serde_json::from_str::<Value>(&response)
                    .unwrap_or_else(|_| Value::String(response));

                let duration_ms = start_time.elapsed().as_millis() as u64;
                return Ok(Observation {
                    output:output.clone(),
                    raw_output:output,
                    is_complete: false,
                    error: None,
                    duration_ms,
                });
            }
            Err(e) => {
                last_error = Some(e);
                continue;
            }
        }
    }

    // 所有重试都失败
    let duration_ms = start_time.elapsed().as_millis() as u64;
    Ok(Observation {
        output: Value::Null,
        raw_output: Value::Null,
        is_complete: false,
        error: Some(format!(
            "AI处理失败（重试{}次后）: {}",
            max_retries,
            last_error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "未知错误".to_string())
        )),
        duration_ms,
    })
}

/// 处理通用工具调用（含日志保存）
pub(crate) async fn handle_generic_tool(
    tool_registry: &Arc<ToolRegistry>,
    chunk_store: &Arc<ChunkStore>,
    _store: &Option<StepStore>,
    tool_name: &str,
    mut parameters: Value,
    call_id: &str,
    start_time: Instant,
) -> Result<Observation> {
    let outcome_result = tool_registry.call_tool(tool_name, parameters.clone()).await;

    let duration_ms = start_time.elapsed().as_millis() as u64;

    let outcome = match outcome_result {
        Ok(o) => o,
        Err(e) => {
            return Ok(Observation {
                output: Value::Null,
                raw_output: Value::Null,
                is_complete: false,
                error: Some(e.to_string()),
                duration_ms,
            });
        }
    };

    // 通过 ChunkStore 处理输出：大文本自动分片，小文本原样透传
    let processed_output = chunk_store.handle(outcome.result.content.clone(), tool_name).await?;

    let error_msg = if outcome.result.is_error {
        Some(extract_error_content(&processed_output))
    } else {
        None
    };

    let raw_obs = Observation {
        output: processed_output,
        raw_output: outcome.result.content,
        is_complete: false,
        error: error_msg,
        duration_ms,
    };

    Ok(raw_obs)
}

/// 从工具输出中提取有意义的错误信息
fn extract_error_content(output: &Value) -> String {
    let text = match output {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| format!("{:?}", other)),
    };

    // 截取前 500 字符，避免给 LLM 喂入过多无用信息
    let max_len = 500;
    if text.len() <= max_len {
        format!("工具执行错误: {}", text)
    } else {
        format!("工具执行错误: {}...", &text[..max_len])
    }
}
