//! 本 crate 的测试桩：假的工具注册表与假的 AI 客户端。
//!
//! 桩放在**使用方**（本 crate），不放 `core` —— 仓库约定。只供 [`crate::tests`] 使用。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use planned_agent_core::ai::types::{
    ChatCompletionRequest, ChatCompletionResponse, Choice, FinishReason, Message, MessageContent,
    MessageRole,
};
use planned_agent_core::ai::{AiClient, ChatCompletionStream};
use planned_agent_core::mcp::types::ToolResult;
use planned_agent_core::tool_registry::ToolRegistryTrait;
use serde_json::{json, Value};

use crate::ScriptHost;

/// 只认识一个 `echo` 工具的假统一入口。
pub(crate) struct FakeRegistry {
    /// `echo` 一律回这个内容。
    pub(crate) result: Value,
    /// 收到过的调用（工具名 + 入参），按顺序。
    pub(crate) calls: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl ToolRegistryTrait for FakeRegistry {
    async fn call_tool(&self, tool_name: &str, arguments: Value) -> anyhow::Result<ToolResult> {
        if tool_name != "echo" {
            anyhow::bail!("未知工具: {tool_name}");
        }
        self.calls
            .lock()
            .expect("calls 锁")
            .push((tool_name.to_string(), arguments));
        Ok(ToolResult {
            call_id: "call-1".to_string(),
            content: self.result.clone(),
            is_error: false,
        })
    }

    fn tool_names(&self) -> Vec<String> {
        vec!["echo".to_string()]
    }
}

/// 固定回答、记录请求的假 AI。
pub(crate) struct FakeAi {
    /// 一律回这个文本。
    pub(crate) reply: String,
    /// 收到过的请求，按顺序。
    pub(crate) requests: Mutex<Vec<ChatCompletionRequest>>,
}

#[async_trait]
impl AiClient for FakeAi {
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> anyhow::Result<ChatCompletionResponse> {
        self.requests.lock().expect("requests 锁").push(request);
        Ok(ChatCompletionResponse {
            id: "fake".to_string(),
            object: "chat.completion".to_string(),
            created: 0,
            model: "fake".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Message {
                    role: MessageRole::Assistant,
                    content: Some(MessageContent::Text {
                        text: self.reply.clone(),
                    }),
                    ..Default::default()
                },
                finish_reason: Some(FinishReason::Stop),
                logprobs: None,
            }],
            usage: None,
            system_fingerprint: None,
        })
    }

    async fn chat_completion_stream(
        &self,
        _request: ChatCompletionRequest,
    ) -> anyhow::Result<ChatCompletionStream> {
        anyhow::bail!("FakeAi 不支持流式调用")
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

/// 造一个已知返回值的假注册表。
pub(crate) fn fake_registry() -> Arc<FakeRegistry> {
    Arc::new(FakeRegistry {
        result: json!({"echoed": true}),
        calls: Mutex::new(Vec::new()),
    })
}

/// 造一个固定回答的假 AI。
pub(crate) fn fake_ai(reply: &str) -> Arc<FakeAi> {
    Arc::new(FakeAi {
        reply: reply.to_string(),
        requests: Mutex::new(Vec::new()),
    })
}

/// 一个注册了 `echo` 假工具、**没接 AI** 的宿主。
pub(crate) fn host() -> (ScriptHost, Arc<FakeRegistry>) {
    let registry = fake_registry();
    (ScriptHost::new(registry.clone()), registry)
}
