//! v2 聊天服务核心实现（公开 API）。
//!
//! 本文件只承载对外暴露的服务入口；后台 driver、历史 / 订阅者 / 命令队列等
//! 内部状态由 `state` / `driver` / `command` 子模块承担。
//!
//! ## 串行队列语义
//!
//! 所有 `send` / `confirm_user_action` 进入同一个命令队列，driver 一次只跑
//! 一个对话（一次 `send` 引发的完整多轮 loop），严格保序。对话处于
//! `awaiting_user_action`（等用户确认卡片）期间到达的 `send` 会排队，
//! 等当前对话结束后再处理。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use planned_agent_ai_manager::AiManager;
use planned_agent_core::ai::types::{Message, MessageContent, MessageRole};
use planned_agent_core::prompt::PromptManager;
use planned_agent_tool_manager::ToolRegistry;
use tokio::sync::{mpsc, oneshot};

use crate::chat::state::Command;
use super::config::ChatConfig;
use crate::chat::driver::driver_loop;
use super::event::{SubscriptionGuard, SubscriptionId, ChatEvent};
use crate::chat::state::{resolve_ai_client, State};
use crate::chat::storage::{ChatHistoryStore, InMemoryStore};
use super::ticket::SendTicket;

// ── 公开服务 ────────────────────────────────────────────────────────────────

/// v2 聊天服务：有状态、后台 loop 驱动、事件订阅。
#[derive(Clone)]
pub struct ChatService<PM: PromptManager + Send + Sync + 'static> {
    state: std::sync::Arc<State<PM>>,
}

impl<PM: PromptManager + Send + Sync + 'static> std::fmt::Debug for ChatService<PM> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatService")
            .field("config", &*self.state.config.lock().unwrap())
            .finish()
    }
}

impl<PM: PromptManager + Send + Sync + 'static> ChatService<PM> {
    /// 通过 `AiManager` + 配置构造（使用默认 `InMemoryStore`）。
    pub fn new(
        ai_manager: AiManager,
        tool_registry: Arc<ToolRegistry>,
        prompt_manager: Arc<PM>,
        config: ChatConfig,
    ) -> Result<Self> {
        let ai_client = resolve_ai_client(&ai_manager, &config)?;
        Ok(Self::from_ai_client(
            ai_client,
            tool_registry,
            prompt_manager,
            config,
        ))
    }

    /// 直接注入 `AiClient` 构造（测试 / 自定义 provider 场景）。
    pub fn from_ai_client(
        ai_client: Arc<dyn planned_agent_core::ai::AiClient>,
        tool_registry: Arc<ToolRegistry>,
        prompt_manager: Arc<PM>,
        config: ChatConfig,
    ) -> Self {
        let store: Arc<dyn ChatHistoryStore> = Arc::new(InMemoryStore);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let state = std::sync::Arc::new(State {
            ai_client,
            tool_registry,
            prompt_manager,
            config: std::sync::Mutex::new(config),
            history: crate::chat::state::History::new(store),
            subscribers: crate::chat::state::Subscribers::new(),
            cmd_tx,
            driver_rx: std::sync::Mutex::new(Some(cmd_rx)),
            driver_started: AtomicBool::new(false),
            run_state: std::sync::Mutex::new(crate::chat::state::RunState::Idle),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        Self { state }
    }

    /// 使用自定义 store 构造。
    pub fn with_store(
        ai_client: Arc<dyn planned_agent_core::ai::AiClient>,
        tool_registry: Arc<ToolRegistry>,
        prompt_manager: Arc<PM>,
        config: ChatConfig,
        store: Arc<dyn ChatHistoryStore>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let state = std::sync::Arc::new(State {
            ai_client,
            tool_registry,
            prompt_manager,
            config: std::sync::Mutex::new(config),
            history: crate::chat::state::History::new(store),
            subscribers: crate::chat::state::Subscribers::new(),
            cmd_tx,
            driver_rx: std::sync::Mutex::new(Some(cmd_rx)),
            driver_started: AtomicBool::new(false),
            run_state: std::sync::Mutex::new(crate::chat::state::RunState::Idle),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        Self { state }
    }
}

impl<PM: PromptManager + Send + Sync + 'static> Drop for ChatService<PM> {
    fn drop(&mut self) {
        let role = match &self.state.config.lock().unwrap().run_id {
            Some(id) => format!("sub_agent(run_id={})", id),
            None => "main_agent".to_string(),
        };
        tracing::info!(
            "ChatService 已销毁 (model={}, role={})",
            self.state.ai_client.model_name(),
            role,
        );
    }
}

impl<PM: PromptManager + Send + Sync + 'static> ChatService<PM> {
    // ── 事件订阅 ──

    pub fn on_chat(&self, handler: impl Fn(ChatEvent) + Send + Sync + 'static) -> SubscriptionId {
        self.state.subscribers.subscribe(handler)
    }

    pub fn on_chat_with_guard(
        &self,
        handler: impl Fn(ChatEvent) + Send + Sync + 'static,
    ) -> SubscriptionGuard {
        let id = self.state.subscribers.subscribe(handler);
        SubscriptionGuard::new(self.state.subscribers.inner_weak(), id)
    }

    pub fn unsubscribe(&self, id: SubscriptionId) {
        self.state.subscribers.unsubscribe(id);
    }

    // ── 发送 ──

    pub fn send(&self, message: Message) -> Result<SendTicket> {
        let (tx, rx) = oneshot::channel();
        self.state
            .cmd_tx
            .send(Command::Send {
                message,
                done: tx,
            })
            .map_err(|_| anyhow!("chat driver 已退出，无法发送消息"))?;
        Ok(SendTicket { rx })
    }

    pub fn send_text(&self, text: impl Into<String>) -> Result<SendTicket> {
        self.send(Message {
            role: MessageRole::User,
            content: Some(MessageContent::Text { text: text.into() }),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        })
    }

    pub fn confirm_user_action(
        &self,
        tool_call_id: &str,
        choice: &str,
        action_id: &str,
    ) -> Result<()> {
        self.state
            .cmd_tx
            .send(Command::Confirm {
                tool_call_id: tool_call_id.to_string(),
                choice: choice.to_string(),
                action_id: action_id.to_string(),
            })
            .map_err(|_| anyhow!("chat driver 已退出，无法提交确认"))?;
        Ok(())
    }

    pub fn resume_sub_agent(&self, run_id: &str, user_input: serde_json::Value) -> Result<()> {
        self.state.tool_registry.signal_resume(run_id, user_input)
    }

    pub(crate) fn resume(&self, choice: &str, action_id: &str) -> Result<SendTicket> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.state
            .cmd_tx
            .send(Command::Resume {
                choice: choice.to_string(),
                action_id: action_id.to_string(),
                done: tx,
            })
            .map_err(|_| anyhow!("chat driver 已退出，无法恢复子 agent"))?;
        Ok(SendTicket { rx })
    }

    // ── 控制与查询 ──

    pub fn stop(&self) {
        self.state.cancelled.store(true, Ordering::SeqCst);
        let count = self.state.tool_registry.clear_sub_agent_sessions();
        if count > 0 {
            tracing::info!(
                "chat: stop() 清理了 {} 个挂起的子 agent 会话",
                count
            );
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::SeqCst)
    }

    pub fn is_awaiting_user_action(&self) -> bool {
        matches!(
            *self.state.run_state.lock().unwrap(),
            crate::chat::state::RunState::AwaitingUserAction
        )
    }

    pub fn history(&self) -> Vec<Message> {
        self.state.history.snapshot()
    }

    /// 当前历史快照（含错误类型元数据，用于 GUI 历史加载）。
    pub fn history_store(&self) -> Vec<crate::chat::storage::StoreMessage> {
        self.state.history.snapshot_store()
    }

    pub fn clear(&self) {
        let state = self.state.run_state.lock().unwrap();
        if matches!(*state, crate::chat::state::RunState::Running | crate::chat::state::RunState::AwaitingUserAction) {
            tracing::warn!("chat: clear() 被调用但对话正在运行（{:?}），跳过清空以避免竞争；请用 reset_session()", *state);
            return;
        }
        drop(state);
        self.state.history.clear();
        self.state.subscribers.emit(ChatEvent::HistoryUpdated {
            messages: self.state.history.snapshot(),
        });
    }

    pub fn set_system_prompt_template(&self, template: Option<String>) {
        self.state.config.lock().unwrap().system_prompt_template = template;
    }

    pub fn set_allowed_tools(&self, allowed: Option<Vec<String>>) {
        self.state.config.lock().unwrap().allowed_tools = allowed;
    }

    pub fn set_run_id(&self, run_id: Option<String>) {
        self.state.config.lock().unwrap().run_id = run_id;
    }

    pub fn reset_session(&self) -> Result<()> {
        self.state
            .cmd_tx
            .send(Command::Reset)
            .map_err(|_| anyhow!("chat driver 已退出，无法重置会话"))?;
        Ok(())
    }

    pub fn prompt_manager(&self) -> Arc<PM> {
        self.state.prompt_manager.clone()
    }

    // ── 内部：driver 生命周期 ──

    pub fn start_driver(&self) -> Result<()> {
        if self
            .state
            .driver_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }
        let rx = self
            .state
            .driver_rx
            .lock()
            .unwrap()
            .take()
            .expect("driver_rx 只应被取出一次");
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(e) => {
                self.state.driver_started.store(false, Ordering::SeqCst);
                *self.state.driver_rx.lock().unwrap() = Some(rx);
                return Err(anyhow!(
                    "无法获取 tokio runtime（{}）：需要运行中的 tokio runtime 驱动后台 loop",
                    e
                ));
            }
        };
        let state = self.state.clone();
        handle.spawn(driver_loop(std::sync::Arc::downgrade(&state), rx));
        Ok(())
    }
}
