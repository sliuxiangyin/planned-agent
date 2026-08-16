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
    /// 通过 `AiManager` + 配置构造。
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
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let state = std::sync::Arc::new(State {
            ai_client,
            tool_registry,
            prompt_manager,
            config: std::sync::Mutex::new(config),
            history: crate::chat::state::History::new(),
            subscribers: crate::chat::state::Subscribers::new(),
            cmd_tx,
            driver_rx: std::sync::Mutex::new(Some(cmd_rx)),
            driver_started: AtomicBool::new(false),
            run_state: std::sync::Mutex::new(crate::chat::state::RunState::Idle),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        Self { state }
    }

    // ── 事件订阅 ──

    /// 注册事件监听，返回订阅 ID（可用 [`Self::unsubscribe`] 取消）。
    ///
    /// 推荐改用 [`Self::on_chat_with_guard`]：返回 RAII 守卫，作用域结束
    /// 时自动退订，从源头避免 handler 闭包 + 其捕获上下文泄漏到已死的生命周期
    /// （典型场景：Dioxus 页面切换、组件卸载、服务端请求结束等）。
    pub fn on_chat(&self, handler: impl Fn(ChatEvent) + Send + Sync + 'static) -> SubscriptionId {
        self.state.subscribers.subscribe(handler)
    }

    /// 注册事件监听，返回 RAII 守卫 [`SubscriptionGuard`]（`Drop` 时自动退订）。
    ///
    /// 推荐把 guard 存放在与业务状态同寿命的作用域：guard drop → handler 退订 →
    /// driver 不再向已死的闭包派发事件，从源头杜绝 handler 闭包 + 其捕获上下文
    /// 跨生命周期残留导致的内存泄漏 + 写入已脱离渲染树的 Signals 等问题。
    ///
    /// guard 不持有 service 的强引用，不延长 service 寿命；service 全部 drop 后
    /// guard 的 Drop 自动成为 no-op。
    pub fn on_chat_with_guard(
        &self,
        handler: impl Fn(ChatEvent) + Send + Sync + 'static,
    ) -> SubscriptionGuard {
        let id = self.state.subscribers.subscribe(handler);
        SubscriptionGuard::new(self.state.subscribers.inner_weak(), id)
    }

    /// 取消事件订阅（按 ID）。
    ///
    /// 若已使用 [`Self::on_chat_with_guard`]，无需手动调用——guard Drop 自动退订。
    pub fn unsubscribe(&self, id: SubscriptionId) {
        self.state.subscribers.unsubscribe(id);
    }

    // ── 发送 ──

    /// 发送单条消息并触发一次异步对话。
    ///
    /// 调用前须确保 [`start_driver`](Self::start_driver) 已调用。
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

    /// 便捷方法：发送一条文本 user 消息。
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

    /// 提交用户对 UI 交互卡片的确认。
    ///
    /// 调用前须确保 [`start_driver`](Self::start_driver) 已调用。
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

    /// 恢复挂起的子 agent（前端驱动 resume 入口）。
    ///
    /// 调用方在收到子 agent 的 `ChatEvent::UIActionRequest`（`session_id` 非空，
    /// 即 run_id）后，用户确认时调用此方法，把选择发给挂起的子 agent。
    ///
    /// 这是**同步发信号**：直接操作 `ToolRegistry` 的挂起会话存储，唤醒阻塞在
    /// `execute_streamed` 中的子 agent 继续执行——不经过本 service 的 driver 队列
    /// （driver 此刻正阻塞在子 agent 工具调用上，无法消费新命令）。
    pub fn resume_sub_agent(&self, run_id: &str, user_input: serde_json::Value) -> Result<()> {
        self.state.tool_registry.signal_resume(run_id, user_input)
    }

    /// 子 agent 内部 resume：压入 tool 消息闭合挂起的 `request_user_action`，
    /// 然后从 history 继续 `run_conversation`（**不是**新 `send`）。
    ///
    /// 由 [`crate::chat::SubAgentSession::resume`] 调用。
    ///
    /// 调用前须确保 [`start_driver`](Self::start_driver) 已调用。
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

    /// 请求取消当前对话（下一个检查点生效）。
    ///
    /// 除设置 `cancelled` 标志外，同时清理所有挂起的子 agent 会话（drop
    /// `resume_tx`），使阻塞在 `execute_streamed` → `rx.await` 的子 agent
    /// 立即收到通道关闭错误并返回，从而解除主 agent driver 的阻塞。
    pub fn stop(&self) {
        self.state.cancelled.store(true, Ordering::SeqCst);
        // 清理所有挂起的子 agent 会话：drop resume_tx 唤醒阻塞的 execute_streamed
        let count = self.state.tool_registry.clear_sub_agent_sessions();
        if count > 0 {
            tracing::info!(
                "chat: stop() 清理了 {} 个挂起的子 agent 会话",
                count
            );
        }
    }

    /// 取消状态查询。
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::SeqCst)
    }

    /// 是否有正在等待用户确认的 UI 卡片。
    pub fn is_awaiting_user_action(&self) -> bool {
        matches!(
            *self.state.run_state.lock().unwrap(),
            crate::chat::state::RunState::AwaitingUserAction
        )
    }

    /// 内部消息历史快照。
    pub fn history(&self) -> Vec<Message> {
        self.state.history.snapshot()
    }

    /// 清空内部消息历史（会话重置）。
    ///
    /// **立即**清空；若对话正在运行（`Running` / `AwaitingUserAction`），本方法
    /// 会发出警告并跳过清空，避免与 driver 竞争。安全的替代方案是
    /// [`Self::reset_session`]（入队串行清空）。
    pub fn clear(&self) {
        let state = self.state.run_state.lock().unwrap();
        if matches!(*state, crate::chat::state::RunState::Running | crate::chat::state::RunState::AwaitingUserAction) {
            tracing::warn!("chat: clear() 被调用但对话正在运行（{:?}），跳过清空以避免竞争；请用 reset_session()", *state);
            return;
        }
        drop(state);
        self.state.history.clear();
    }

    /// 热切换 system prompt 模板（下次 `send` / `reset_session` 后生效）。
    ///
    /// 仅更新配置，**不**触碰历史；配合 [`Self::reset_session`] 使用可在
    /// 不重建 service 的前提下切换模板（新模板会在历史清空后的首次
    /// `send` 时注入）。
    pub fn set_system_prompt_template(&self, template: Option<String>) {
        self.state.config.lock().unwrap().system_prompt_template = template;
    }

    /// 热切换工具白名单（下次 `send` 时立即生效）。
    ///
    /// - `None`：全部工具可用
    /// - `Some(names)`：仅白名单中的工具暴露给 LLM
    pub fn set_allowed_tools(&self, allowed: Option<Vec<String>>) {
        self.state.config.lock().unwrap().allowed_tools = allowed;
    }

    /// 注入本次执行的 run_id（子 agent 每次 `start` 前调用）。
    ///
    /// - `None`：主 agent
    /// - `Some(invocation_id)`：子 agent，决定 `run_conversation` 的 UI 策略
    ///   （挂起返回 vs 阻塞确认），并作为前端 resume 的路由键。
    pub fn set_run_id(&self, run_id: Option<String>) {
        self.state.config.lock().unwrap().run_id = run_id;
    }

    /// 会话重置（入队、串行安全）：清空内部消息历史。
    ///
    /// 与 [`Self::clear`] 的区别：本方法把清空动作放入命令队列，由 driver
    /// 在**当前对话（含 UI 确认等待）结束后**执行，因此即使有对话正在运行
    /// 也安全——配合 `stop()` + [`Self::set_system_prompt_template`] 即可
    /// 实现"不重建 service 的模板热切换"。
    ///
    /// 调用前须确保 [`start_driver`](Self::start_driver) 已调用。
    pub fn reset_session(&self) -> Result<()> {
        self.state
            .cmd_tx
            .send(Command::Reset)
            .map_err(|_| anyhow!("chat driver 已退出，无法重置会话"))?;
        Ok(())
    }

    /// 获取 PromptManager 引用（供外部复用）。
    pub fn prompt_manager(&self) -> Arc<PM> {
        self.state.prompt_manager.clone()
    }

    // ── 内部：driver 生命周期 ──

    /// 启动后台 driver task（幂等；多次调用安全）。
    ///
    /// 首次 `send` / `confirm_user_action` / `reset_session` 前必须调用。
    /// 主 agent 在 service ready 后调用一次；子 agent 在 `start()` 时调用。
    pub fn start_driver(&self) -> Result<()> {
        // 使用 compare_exchange 确保只有第一个调用者负责启动 driver
        if self
            .state
            .driver_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            // 已经启动过，直接返回
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
                // 启动失败，回滚 driver_started 状态，允许后续重试
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