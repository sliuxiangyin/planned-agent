//! 工具执行桥接策略。

use crate::chat::service::ChatEvent;
use crate::chat::state::State;
use planned_agent_tool_manager::ToolStreamSender;
use std::sync::Arc;

pub(crate) trait ToolExecutionBridge: Send + Sync {
    fn needs_stream(&self, tool_name: &str) -> bool;
    fn create_stream(
        &self,
        tool_name: &str,
        call_id: &str,
    ) -> (ToolStreamSender, tokio::task::JoinHandle<()>);
}

pub(crate) struct SubAgentBridge<
    PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static,
> {
    state: Arc<State<PM>>,
}

impl<PM: planned_agent_core::prompt::PromptManager + Send + Sync + 'static> SubAgentBridge<PM> {
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
        // 把本层（父）本地取消 watch 作为下游子 agent 的上游取消，实现级联取消。
        let stream = ToolStreamSender::new(stream_tx, tool_name.to_string(), call_id.to_string())
            .with_upstream(self.state.cancel_rx());

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
