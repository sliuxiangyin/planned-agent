//! flexible 模块的测试桩：脚本化假 AI 客户端、假工具、事件记录 sink。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use planned_agent_core::ai::types::{
    ChatCompletionRequest, ChatCompletionResponse, Choice, FinishReason, FunctionCall, Message,
    MessageContent, MessageRole, ToolCall, ToolType, Usage,
};
use planned_agent_core::ai::{AiClient, ChatCompletionStream};
use planned_agent_core::mcp::types::ToolResult;
use planned_agent_core::tool_registry::ToolExecutor;
use serde_json::Value;

use super::event::{PlanRunEvent, PlanRunSink};

/// 脚本化的假 AI 客户端：按调用顺序返回预设响应，并记录收到的请求。
pub(crate) struct FakeAiClient {
    responses: Mutex<VecDeque<ChatCompletionResponse>>,
    requests: Mutex<Vec<ChatCompletionRequest>>,
}

impl FakeAiClient {
    /// 用预设响应构造（按调用顺序消费）。
    pub(crate) fn new(responses: Vec<ChatCompletionResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// 已收到的请求（按顺序）。
    pub(crate) fn requests(&self) -> Vec<ChatCompletionRequest> {
        self.requests.lock().expect("requests 锁").clone()
    }
}

#[async_trait]
impl AiClient for FakeAiClient {
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        self.requests.lock().expect("requests 锁").push(request);
        self.responses
            .lock()
            .expect("responses 锁")
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("FakeAiClient 脚本已用尽"))
    }

    async fn chat_completion_stream(
        &self,
        _request: ChatCompletionRequest,
    ) -> Result<ChatCompletionStream> {
        anyhow::bail!("FakeAiClient 不支持流式调用")
    }

    fn provider_name(&self) -> &str {
        "fake"
    }

    fn model_name(&self) -> &str {
        "fake-model"
    }

    fn default_config(&self) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "fake-model".to_string(),
            messages: vec![],
            tools: None,
            temperature: None,
            max_tokens: None,
            stream: false,
            extra: Default::default(),
        }
    }
}

/// 构造"纯文本回答"的响应。
pub(crate) fn text_response(
    content: &str,
    prompt_tokens: u32,
    completion_tokens: u32,
) -> ChatCompletionResponse {
    response(
        assistant_message(Some(content), None),
        Some(usage(prompt_tokens, completion_tokens)),
    )
}

/// 构造"要求调用工具"的响应。
pub(crate) fn tool_response(
    call_id: &str,
    tool: &str,
    arguments: Value,
    prompt_tokens: u32,
    completion_tokens: u32,
) -> ChatCompletionResponse {
    let call = ToolCall {
        id: call_id.to_string(),
        r#type: ToolType::Function,
        function: FunctionCall {
            name: tool.to_string(),
            arguments: arguments.to_string(),
        },
    };
    response(
        assistant_message(None, Some(vec![call])),
        Some(usage(prompt_tokens, completion_tokens)),
    )
}

fn response(message: Message, usage: Option<Usage>) -> ChatCompletionResponse {
    ChatCompletionResponse {
        id: "fake-completion".to_string(),
        object: "chat.completion".to_string(),
        created: 0,
        model: "fake-model".to_string(),
        choices: vec![Choice {
            index: 0,
            message,
            finish_reason: Some(FinishReason::Stop),
            logprobs: None,
        }],
        usage,
        system_fingerprint: None,
    }
}

fn assistant_message(content: Option<&str>, tool_calls: Option<Vec<ToolCall>>) -> Message {
    Message {
        role: MessageRole::Assistant,
        content: content.map(|text| MessageContent::Text {
            text: text.to_string(),
        }),
        tool_calls,
        ..Default::default()
    }
}

fn usage(prompt_tokens: u32, completion_tokens: u32) -> Usage {
    Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
    }
}

/// 记录调用、执行时返回固定输出的假工具。
pub(crate) struct FakeTool {
    name: String,
    output: Value,
    is_error: bool,
    /// 每次调用的 `(tool_name, arguments)`。
    pub(crate) calls: Mutex<Vec<(String, Value)>>,
}

/// 造一个假工具：返回的 `Arc` 既可注册进 registry，也可事后查调用记录。
pub(crate) fn fake_tool(name: &str, output: Value, is_error: bool) -> Arc<FakeTool> {
    Arc::new(FakeTool {
        name: name.to_string(),
        output,
        is_error,
        calls: Mutex::new(Vec::new()),
    })
}

#[async_trait]
impl ToolExecutor for FakeTool {
    async fn execute(&self, tool_name: &str, arguments: Value) -> Result<ToolResult> {
        self.calls
            .lock()
            .expect("calls 锁")
            .push((tool_name.to_string(), arguments));
        Ok(ToolResult {
            call_id: String::new(),
            content: self.output.clone(),
            is_error: self.is_error,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "测试用假工具"
    }

    fn supported_tools(&self) -> Vec<String> {
        vec![self.name.clone()]
    }

    fn supports_tool(&self, name: &str) -> bool {
        name == self.name
    }
}

/// 记录所有事件的 sink。
#[derive(Default)]
pub(crate) struct RecordingSink {
    events: Mutex<Vec<PlanRunEvent>>,
}

impl RecordingSink {
    /// 取已记录的事件（克隆）。
    pub(crate) fn events(&self) -> Vec<PlanRunEvent> {
        self.events.lock().expect("events 锁").clone()
    }
}

impl PlanRunSink for RecordingSink {
    fn emit(&self, event: PlanRunEvent) {
        self.events.lock().expect("events 锁").push(event);
    }
}
