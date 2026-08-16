//! 工具执行桥接策略。
//!
//! 不同类型的工具（普通工具、子 agent）需要不同的执行方式：
//! - 普通工具：直接 `call_tool`，无流式旁路；
//! - 子 agent：通过 `ToolStreamSender` 创建 mpsc 旁路，实时转发事件。

use crate::chat::service::ChatEvent;
use crate::chat::state::State;
use planned_agent_tool_manager::ToolStreamSender;
use std::sync::Arc;

/// 工具执行桥接策略。
///
/// 判断一个工具调用是否需要流式旁路（如子 agent），
/// 并在需要时创建流通道和后台转发任务。
pub(crate) trait ToolExecutionBridge: Send + Sync {
    /// 判断该工具调用是否需要流式旁路。
    fn needs_stream(&self, tool_name: &str) -> bool;

    /// 创建流式旁路：返回 `(sender, relay_handle)`。
    ///
    /// - `sender`：传给 `call_tool_streamed`，子 agent 通过它发射事件；
    /// - `relay_handle`：后台任务，把 sender 收到的事件转发到订阅者。
    ///   调用方应在 `call_tool_streamed` 返回后 `.await` 此 handle。
    fn create_stream(
        &self,
        tool_name: &str,
        call_id: &str,
    ) -> (ToolStreamSender, tokio::task::JoinHandle<()>);
}

/// 子 agent 桥接：创建 mpsc 通道，spawn 转发任务到父 agent subscribers。
pub(crate) struct SubAgentBridge<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static>
{
    state: Arc<State<PM>>,
}

impl<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static>
    SubAgentBridge<PM>
{
    pub fn new(state: Arc<State<PM>>) -> Self {
        Self { state }
    }
}

impl<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static> ToolExecutionBridge
    for SubAgentBridge<PM>
{
    fn needs_stream(&self, tool_name: &str) -> bool {
        self.state
            .tool_registry
            .get_metadata(tool_name)
            .map(|m| {
                matches!(
                    m.source,
                    planned_agent_tool_manager::ToolSource::SubAgent { .. }
                )
            })
            .unwrap_or(false)
    }

    fn create_stream(
        &self,
        tool_name: &str,
        call_id: &str,
    ) -> (ToolStreamSender, tokio::task::JoinHandle<()>) {
        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::channel(64);
        let stream = ToolStreamSender::new(stream_tx, tool_name.to_string(), call_id.to_string());

        let state_clone = self.state.clone();
        let call_id_owned = call_id.to_string();
        let handle = tokio::spawn(async move {
            while let Some(ev) = stream_rx.recv().await {
                if let Some(chat_ev) = ev.event {
                    state_clone.subscribers.emit(ChatEvent::Chat(chat_ev));
                }
            }
            tracing::info!("[bridge] 子 agent stream 结束, call_id={}", call_id_owned);
        });

        (stream, handle)
    }
}
