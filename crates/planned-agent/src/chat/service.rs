//! 聊天服务核心实现
//!
//! `ChatService` 负责接收调用方传入的历史 `Vec<Message>`,通过注入的 `Arc<PromptManager>`
//! 渲染 system prompt 模板（路径由 `ChatConfig::system_prompt_template` 决定），
//! 通过 `AiClient` 发送流式请求,处理多轮 tool-call 循环。
//!
//! ## 设计风格：参照 `LlmCoarsePlanner::generate_coarse_plan_stream`
//!
//! [`ChatService::chat_with_callback`] 是 **async 函数 + 同步 `FnMut` 回调**，
//! 不再返回 `impl Stream<Item = Result<ChatEvent>>`。
//!
//! 关键点：
//! - 每个 AI chunk 在 `inner.next().await` 拿到后**立即**通过 `on_event` 回调下发，
//!   runtime 可正常 yield → Dioxus 等上层 UI 的 signal 写入立刻生效，不会被阻塞。
//! - 没有 `futures::executor::block_on`——之前的实现因为在 `poll_next` 里同步跑
//!   `block_on` 把整个 runtime 线程卡住，导致 GUI 看不到流式过程。
//! - 没有自实现 `Stream` trait 的状态机（`ChatStream` / `StreamState`），
//!   改用普通 `loop` 驱动多轮。
//!
//! 调用方在 `on_event` 内可以直接写 signal，不需要 spawn 或 channel。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use futures::StreamExt;
use planned_agent_ai_manager::AiManager;
use planned_agent_core::ai::AiClient;
use planned_agent_core::prompt::{PromptContext, PromptManager};
use planned_agent_core::types::{
    ChatCompletionRequest, FinishReason, FunctionCall, Message, MessageContent, MessageRole,
    ToolCall, ToolDefinition, ToolType, UIAction,
};
use planned_agent_tool_manager::ToolRegistry;
use serde_json::Value;
use tracing::{info, warn};

use crate::chat::config::ChatConfig;
use crate::chat::event::ChatEvent;

// ── 公开类型 ─────────────────────────────────────────────────────────────────

/// 单轮对话的完整响应。
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// 最终 assistant 消息。
    ///
    /// 若因 `max_tool_rounds` 截断,此消息可能仍含未执行的 `tool_calls`。
    pub message: Message,
    /// 完整消息历史(含 system / user / assistant(含 tool_calls) / tool 等
    /// 所有中间消息)。
    pub history: Vec<Message>,
    /// 本次聊天中实际执行的 tool 调用次数(含失败)。
    pub tool_calls_executed: usize,
    /// 最后一轮 assistant 的结束原因。
    pub finish_reason: Option<FinishReason>,
    /// 待处理的 UI 交互动作。非空时表示需要用户操作后才能继续对话。
    pub pending_ui_actions: Vec<PendingUIAction>,
    /// 是否被用户主动取消。
    pub cancelled: bool,
}

/// 待处理的 UI 交互请求。
///
/// 当 Agent 调用 `request_user_action` tool 时，`ChatService` 不会执行该工具，
/// 而是将其封装为此结构返回给调用方，由调用方渲染 UI 并收集用户输入。
#[derive(Debug, Clone)]
pub struct PendingUIAction {
    /// 展示给用户的引导消息
    pub message: String,
    /// 用户可选的动作列表
    pub actions: Vec<UIAction>,
}

/// 聊天服务。
///
/// 由调用方在外部组装好 `AiManager` + `ToolRegistry` + `Arc<PM>` + `ChatConfig` 后创建。
/// 本身不持有任何独占锁,内部组件各自线程安全。
///
/// `PM: PromptManager` 与 [`crate::planner::coarse::LlmCoarsePlanner`] 走相同泛型套路，
/// 方便复用同一种 `PromptManager`（如 `FilePromptManager`）渲染 system prompt 模板，
/// 渲染路径与 `LlmCoarsePlanner::generate_coarse_plan_stream` 中 `pm.render(name, ctx)` 一致。
#[derive(Clone)]
pub struct ChatService<PM: PromptManager + Send + Sync + 'static> {
    ai_manager: AiManager,
    tool_registry: Arc<ToolRegistry>,
    prompt_manager: Arc<PM>,
    config: ChatConfig,
    cancelled: Arc<AtomicBool>,
}

impl<PM: PromptManager + Send + Sync + 'static> std::fmt::Debug for ChatService<PM> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatService")
            .field("config", &self.config)
            .finish()
    }
}

// ── 实现 ─────────────────────────────────────────────────────────────────────

impl<PM: PromptManager + Send + Sync + 'static> ChatService<PM> {
    /// 创建 `ChatService`。
    ///
    /// `prompt_manager` 必传：system prompt 渲染依赖它（参见
    /// [`ChatConfig::system_prompt_template`]）。通常传入 `Arc<FilePromptManager>`
    /// 或同 trait 的 Mock 实现。
    pub fn new(
        ai_manager: AiManager,
        tool_registry: Arc<ToolRegistry>,
        prompt_manager: Arc<PM>,
        config: ChatConfig,
    ) -> Self {
        Self {
            ai_manager,
            tool_registry,
            prompt_manager,
            config,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 覆盖 `prompt_manager`（链式调用）。
    ///
    /// 适用于测试场景：先用最小 `ChatService::new(... mock_pm, config)` 构建，
    /// 再覆盖为真实 `FilePromptManager`；或反过来。生产路径通常不调用本方法。
    pub fn with_prompt_manager(mut self, pm: Arc<PM>) -> Self {
        self.prompt_manager = pm;
        self
    }

    /// 派生一个仅 `allowed_tools` 不同的副本。
    ///
    /// 灵活模式通过此方法在清晰度检查阶段限制工具为 `["request_user_action"]`，
    /// 确认需求明确后再用完整工具集执行。内部组件（AiManager、ToolRegistry、
    /// PromptManager）均为浅拷贝，开销极小。
    pub fn with_allowed_tools(&self, allowed_tools: Option<Vec<String>>) -> Self {
        Self {
            ai_manager: self.ai_manager.clone(),
            tool_registry: self.tool_registry.clone(),
            prompt_manager: self.prompt_manager.clone(),
            config: ChatConfig {
                allowed_tools,
                ..self.config.clone()
            },
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 派生一个仅 `system_prompt_template` 不同的副本。
    ///
    /// 灵活模式 Phase 1（清晰度检查）与 Phase 2（执行）使用不同的 system prompt。
    pub fn with_system_prompt_template(&self, template: Option<String>) -> Self {
        Self {
            ai_manager: self.ai_manager.clone(),
            tool_registry: self.tool_registry.clone(),
            prompt_manager: self.prompt_manager.clone(),
            config: ChatConfig {
                system_prompt_template: template,
                ..self.config.clone()
            },
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 请求取消当前正在进行的聊天流。
    ///
    /// 调用后 `chat_with_callback` 会在下一个 stream chunk 或下一轮开始时
    /// 停止并返回 `ChatResponse { cancelled: true, .. }`。
    pub fn stop(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// 内部取消检查。
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// 流式聊天入口（粗粒度风格）。
    ///
    /// 接收调用方维护的消息历史,**实时**下发 [`ChatEvent`] 给回调 `on_event`：
    /// - 每收到一段 AI chunk → 立即 `TextDelta(chunk)` / `ReasoningDelta(chunk)`
    /// - 每个 tool_call 首次拿到 `id` → `ToolCallStart`
    /// - 每个 tool_call 后续拿到 `arguments` 增量 → `ToolCallArgsDelta`
    /// - 一轮 tool_calls 累积完成 → `ToolCallComplete`（含完整 JSON `arguments`）
    /// - 每个工具执行完 → `ToolExecuted`
    /// - 每轮结束 → `RoundEnd`；新轮开始 → `RoundStart`
    ///
    /// `on_event` 是 **同步** `FnMut`，调用方可在闭包内直接写 signal / DOM，
    /// 不需要 spawn 或 channel 中转。
    ///
    /// **不**通过 `ChatEvent::Done` 通知完成——正常结束通过 `Ok(ChatResponse)`，
    /// 异常通过 `Err`。调用方自行决定如何处理。
    ///
    /// 多轮 tool-call 循环：当 AI 返回的 message 含 `tool_calls` 时，
    /// 本方法会执行这些工具、把 tool message 追加到历史、再发起下一轮，
    /// 直到 AI 不再要求 tool 调用，或达到 [`ChatConfig::max_tool_rounds`]。
    pub async fn chat_with_callback<F>(
        &self,
        messages: Vec<Message>,
        mut on_event: F,
    ) -> Result<ChatResponse>
    where
        F: FnMut(ChatEvent) + Send,
    {
        self.cancelled.store(false, Ordering::SeqCst);

        let ai_client = self.resolve_ai_client()?;

        let mut history = messages;
        self.inject_system_prompt(&mut history).await?;

        let mut tool_calls_executed = 0usize;
        let mut finish_reason: Option<FinishReason> = None;
        let mut round = 1usize;
        let mut pending_ui_actions: Vec<PendingUIAction> = Vec::new();

        loop {
            if self.is_cancelled() {
                break;
            }

            on_event(ChatEvent::RoundStart { round });

            // ── 构造请求 ──
            let tools = self.build_tool_definitions();
            let req = ChatCompletionRequest {
                model: ai_client.model_name().to_string(),
                messages: history.clone(),
                tools: Some(tools),
                temperature: self.config.temperature,
                max_tokens: self.config.max_tokens,
                stream: true,
                extra: Default::default(),
            };

            // ── 发起流式请求 + 消费 chunks ──
            let response_stream = ai_client.chat_completion_stream(req).await?;
            let mut inner = response_stream.stream;

            let mut text = String::new();
            let mut reasoning = String::new();
            let mut has_content = false;
            let mut has_reasoning = false;
            let mut accumulators: HashMap<u32, ToolCallAccumulator> = HashMap::new();
            let mut finish_reason_this_round: Option<FinishReason> = None;

            while let Some(chunk_result) = inner.next().await {
                if self.is_cancelled() {
                    drop(inner);
                    break;
                }

                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("Stream chunk error: {}", e);
                        continue;
                    }
                };

                for choice in chunk.choices {
                    let delta = &choice.delta;

                    // 文本 / 推理：立刻下发 delta
                    if let Some(t) = &delta.content {
                        if !t.is_empty() {
                            has_content = true;
                            text.push_str(t);
                            on_event(ChatEvent::TextDelta(t.clone()));
                        }
                    }
                    if let Some(r) = &delta.reasoning_content {
                        if !r.is_empty() {
                            has_reasoning = true;
                            reasoning.push_str(r);
                            on_event(ChatEvent::ReasoningDelta(r.clone()));
                        }
                    }

                    // tool_calls：累积 + 下发 Start / ArgsDelta
                    if let Some(deltas) = &delta.tool_calls {
                        for d in deltas {
                            let index = d.index;
                            let acc = accumulators
                                .entry(index)
                                .or_insert_with(ToolCallAccumulator::new);

                            // id 第一次设上 → 发 ToolCallStart
                            if let Some(id) = &d.id {
                                if acc.id.is_empty() {
                                    acc.id.clone_from(id);
                                }
                            }
                            // name 第一次设上
                            if let Some(func) = &d.function {
                                if let Some(name) = &func.name {
                                    if acc.name.is_empty() {
                                        acc.name.clone_from(name);
                                    }
                                }
                                // arguments 累积 + 下发 ArgsDelta
                                if let Some(args) = &func.arguments {
                                    if !args.is_empty() {
                                        acc.arguments.push_str(args);
                                        // 仅在 id 已确定后下发（与 Start 配对）
                                        if !acc.id.is_empty() {
                                            on_event(ChatEvent::ToolCallArgsDelta {
                                                id: acc.id.clone(),
                                                delta: args.clone(),
                                            });
                                        }
                                    }
                                }
                            }
                            // Start 仅发一次（id + name 都已确定时）
                            if !acc.id.is_empty() && !acc.name.is_empty() && !acc.start_emitted {
                                on_event(ChatEvent::ToolCallStart {
                                    id: acc.id.clone(),
                                    name: acc.name.clone(),
                                });
                                acc.start_emitted = true;
                            }
                        }
                    }

                    if choice.finish_reason.is_some() && finish_reason_this_round.is_none() {
                        finish_reason_this_round = choice.finish_reason.clone();
                    }
                }
            }

            // ── 本轮 tool_calls 累积完成 → 发 Complete ──
            let tool_calls_vec: Vec<ToolCall> = accumulators
                .values()
                .filter(|acc| !acc.id.is_empty())
                .map(|acc| ToolCall {
                    id: acc.id.clone(),
                    r#type: ToolType::Function,
                    function: FunctionCall {
                        name: acc.name.clone(),
                        arguments: acc.arguments.clone(),
                    },
                })
                .collect();

            for acc in accumulators.values() {
                if acc.id.is_empty() {
                    continue;
                }
                let arguments: Value = serde_json::from_str(&acc.arguments)
                    .unwrap_or_else(|_| Value::String(acc.arguments.clone()));
                on_event(ChatEvent::ToolCallComplete {
                    id: acc.id.clone(),
                    name: acc.name.clone(),
                    arguments,
                });
            }

            // ── 构造 assistant_msg 并 push 到历史 ──
            let assistant_msg = Message {
                role: MessageRole::Assistant,
                content: if has_content {
                    Some(MessageContent::Text { text })
                } else {
                    None
                },
                tool_calls: if tool_calls_vec.is_empty() {
                    None
                } else {
                    Some(tool_calls_vec.clone())
                },
                tool_call_id: None,
                name: None,
                reasoning_content: if has_reasoning {
                    Some(reasoning)
                } else {
                    None
                },
            };

            history.push(assistant_msg.clone());
            on_event(ChatEvent::RoundEnd {
                message: assistant_msg,
            });

            finish_reason = finish_reason_this_round.or(finish_reason);

            // ── 无 tool_calls 或达到 max_tool_rounds → 结束 ──
            if tool_calls_vec.is_empty() {
                break;
            }
            if round >= self.config.max_tool_rounds {
                warn!(
                    "chat_with_callback: 达到 max_tool_rounds={}, 终止循环",
                    self.config.max_tool_rounds
                );
                finish_reason = None; // 截断标记

                // 通知用户工具调用已达上限，请求确认是否继续
                let msg = format!(
                    "工具调用已达到最大限制（{} 轮），任务可能未完成。是否继续执行？",
                    self.config.max_tool_rounds
                );
                let actions = vec![
                    UIAction {
                        id: "continue".to_string(),
                        action_type: planned_agent_core::types::UIActionType::Confirm,
                        label: "继续执行".to_string(),
                        description: None,
                        options: vec![],
                    },
                    UIAction {
                        id: "stop".to_string(),
                        action_type: planned_agent_core::types::UIActionType::Confirm,
                        label: "结束".to_string(),
                        description: None,
                        options: vec![],
                    },
                ];
                on_event(ChatEvent::UIActionRequest {
                    message: msg.clone(),
                    actions: actions.clone(),
                });
                pending_ui_actions.push(PendingUIAction {
                    message: msg,
                    actions,
                });
                break;
            }

            // ── 分离 UI 工具和普通后端工具 ──
            const UI_TOOL_NAMES: &[&str] = &["request_user_action"];

            let (ui_calls, backend_calls): (Vec<_>, Vec<_>) = tool_calls_vec
                .iter()
                .partition(|tc| UI_TOOL_NAMES.contains(&tc.function.name.as_str()));

            // 先执行普通后端工具（现有逻辑不变）
            for call in &backend_calls {
                let args: Value = serde_json::from_str(&call.function.arguments)
                    .unwrap_or_else(|_| Value::String(call.function.arguments.clone()));

                let outcome = self.tool_registry.call_tool(&call.function.name, args).await;
                let (is_error, content) = match &outcome {
                    Ok(o) => (o.result.is_error, o.result.content.clone()),
                    Err(e) => {
                        warn!("Tool '{}' failed: {}", call.function.name, e);
                        (true, Value::String(format!("Error: {}", e)))
                    }
                };

                history.push(Message {
                    role: MessageRole::Tool,
                    content: Some(MessageContent::ToolResult {
                        tool_call_id: call.id.clone(),
                        content: serde_json::to_string(&content)
                            .unwrap_or_else(|_| content.to_string()),
                    }),
                    tool_call_id: Some(call.id.clone()),
                    tool_calls: None,
                    name: None,
                    reasoning_content: None,
                });

                if outcome.is_ok() {
                    tool_calls_executed += 1;
                }

                on_event(ChatEvent::ToolExecuted {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    is_error,
                    content,
                });
            }

            // 🆕 处理 UI 工具：不执行后端逻辑，发出 UIActionRequest 事件 +
            // 推送占位 tool result（保持 assistant→tool 消息顺序，前端后续替换）
            for call in &ui_calls {
                let args: Value = serde_json::from_str(&call.function.arguments)
                    .unwrap_or_default();

                let message = args["message"].as_str().unwrap_or("").to_string();
                let actions: Vec<UIAction> = serde_json::from_value(
                    args.get("actions").cloned().unwrap_or(Value::Array(vec![])),
                )
                .unwrap_or_default();

                on_event(ChatEvent::UIActionRequest {
                    message: message.clone(),
                    actions: actions.clone(),
                });

                // 占位 tool result —— 前端在用户操作后替换为真实结果
                history.push(Message {
                    role: MessageRole::Tool,
                    content: Some(MessageContent::ToolResult {
                        tool_call_id: call.id.clone(),
                        content: r#"{"status":"awaiting_user_input"}"#.to_string(),
                    }),
                    tool_call_id: Some(call.id.clone()),
                    tool_calls: None,
                    name: None,
                    reasoning_content: None,
                });

                pending_ui_actions.push(PendingUIAction { message, actions });
            }

            // 有 UI action 则中断循环（等用户操作后前端重新调用 chat_with_callback）
            if !ui_calls.is_empty() {
                break;
            }

            round += 1;
        }

        // ── 构造最终 response ──
        let message = history
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::Assistant))
            .cloned()
            .ok_or_else(|| {
                anyhow!("chat_with_callback: history 不包含任何 assistant 消息")
            })?;

        Ok(ChatResponse {
            message,
            history,
            tool_calls_executed,
            finish_reason,
            pending_ui_actions,
            cancelled: self.is_cancelled(),
        })
    }

    /// 解析 AI 客户端。
    fn resolve_ai_client(&self) -> Result<Arc<dyn AiClient>> {
        match &self.config.provider {
            Some(name) => self.ai_manager.get(name),
            None => self.ai_manager.default(),
        }
    }

    /// 检查历史首条是否为 System 角色。
    fn first_message_is_system(history: &[Message]) -> bool {
        history
            .first()
            .map(|m| matches!(m.role, MessageRole::System))
            .unwrap_or(false)
    }

    /// 渲染并注入 system prompt 到历史首部(仅当首条非 System 时)。
    ///
    /// 渲染路径与 [`crate::planner::coarse::LlmCoarsePlanner::generate_coarse_plan_stream`]
    /// 完全一致：`pm.render(template, &ctx)`（参见 llm_planner.rs:154-156）。
    /// 当前 `PromptContext` 为空 —— `chat/thorough_system.toml` 的 `context` 变量定义为
    /// `required = false`，可正常渲染；将来若需要给 system 注入动态上下文，
    /// 由 caller 在 `ChatConfig` 之外另行拼装 `PromptContext` 并扩展本函数签名。
    ///
    /// 模板缺失或渲染失败时 `Err` 向上传播 —— 这是破坏性变更，但语义更明确：
    /// caller 配置了模板路径就应当能渲染成功，否则说明配置错配。
    async fn inject_system_prompt(&self, history: &mut Vec<Message>) -> Result<()> {
        let Some(template) = &self.config.system_prompt_template else {
            return Ok(());
        };
        if Self::first_message_is_system(history) {
            return Ok(());
        }

        let rendered = self
            .prompt_manager
            .render(
                template,
                &PromptContext::new().with_variable("context", Value::String(String::new())),
            )
            .await?;

        history.insert(
            0,
            Message {
                role: MessageRole::System,
                content: Some(MessageContent::Text { text: rendered }),
                tool_calls: None,
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            },
        );
        info!("Injected system prompt from template '{}'", template);
        Ok(())
    }

    /// 从 ToolRegistry 构建 ToolDefinition 列表，按 `allowed_tools` 白名单过滤。
    ///
    /// - `allowed_tools = None`：全部工具可用
    /// - `allowed_tools = Some(names)`：仅白名单中的工具会暴露给 LLM
    fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        let all = self.tool_registry.get_all_tools();
        let filtered: Vec<_> = match &self.config.allowed_tools {
            None => all,
            Some(whitelist) => all
                .into_iter()
                .filter(|t| whitelist.contains(&t.name))
                .collect(),
        };
        filtered
            .into_iter()
            .map(|t| ToolDefinition {
                r#type: ToolType::Function,
                function: planned_agent_core::types::FunctionDefinition {
                    name: t.name,
                    description: Some(t.description),
                    parameters: Some(t.input_schema),
                    strict: None,
                },
            })
            .collect()
    }

    /// 从执行轨迹总结生成粗粒度计划（灵活模式）。
    ///
    /// 内部构造 `LlmCoarsePlanner` 并调用 `generate_from_trace`，
    /// 返回序列化后的 `CoarseGrainedPlan` JSON 字符串。
    ///
    /// 调用方无需直接依赖 `planned_agent_core::planner::coarse` 类型。
    pub async fn generate_coarse_plan_from_trace(
        &self,
        trace_summary: &str,
    ) -> Result<String> {
        use crate::planner::coarse::LlmCoarsePlanner;

        let ai_client = self.resolve_ai_client()?;
        let planner = LlmCoarsePlanner::new(ai_client, self.prompt_manager.clone());
        let plan = planner.generate_from_trace(trace_summary).await?;
        let json = serde_json::to_string(&plan)
            .map_err(|e| anyhow!("序列化 CoarseGrainedPlan 失败: {}", e))?;
        Ok(json)
    }
}

// ── 工具调用累积器 ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
    /// 是否已下发过 `ToolCallStart`
    start_emitted: bool,
}

impl ToolCallAccumulator {
    fn new() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            arguments: String::new(),
            start_emitted: false,
        }
    }
}
