//! v2 聊天事件协议
//!
//! [`ChatEvent`] 在复用 core 层 [`ChatEvent`]（流式增量 / 工具执行 /
//! UI 交互请求等）的基础上，补充了 v2 后台 loop 需要而 v1 没有的生命周期
//! 事件：`Done`（一次 `send` 引发的整段对话结束）与 `Error`（可终止性异常）。
//! v1 中正常完成通过 `chat_with_callback` 的返回值表达、异常通过 `Err`；
//! v2 中对话由后台 task 驱动、调用方只能通过事件通道感知结果，因此
//! `Done` / `Error` 是必需的生命周期信号。

use std::sync::Weak;

use planned_agent_core::ai::types::Message;
use planned_agent_core::events::ChatEvent as CoreChatEvent;

use crate::chat::state::SubscribersInner;

/// 聊天事件（v2 协议）。
///
/// 一次 `send` 引发的完整对话通常对应：
/// `Chat(RoundStart)` → `Chat(TextDelta/ReasoningDelta/ToolCall*...)` →
/// `Chat(RoundEnd)` → …（多轮工具调用 / UI 确认）… → `Done { cancelled: false }`。
///
/// 若中途用户 `stop()`，loop 在下一个检查点退出，发出 `Done { cancelled: true }`；
/// 若 LLM 请求等不可恢复错误发生，发出 `Error`（此时不会再有 `Done`）。
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// 复用 core 流式事件（文本/推理增量、工具调用、`UIActionRequest`、
    /// `RoundEnd` 等），语义与 v1 一致。
    Chat(CoreChatEvent),
    /// 一次 `send` 引发的整段对话结束（回到 idle）。
    ///
    /// `cancelled = true` 表示是用户 `stop()` 主动中断，而非自然完成。
    /// 此后调用方可以安全地再次 `send`。
    Done {
        /// 是否被用户主动取消。
        cancelled: bool,
    },
    /// 对话内部的可终止性异常（LLM 请求失败、channel 关闭等）。
    ///
    /// 工具执行失败**不**产生此事件（作为 `ChatEvent::ToolExecuted`
    /// 且 `is_error=true` 的 tool 消息继续对话）；UI 确认等待期间的
    /// 非法调用（如 tool_call_id 不匹配）也走此通道提示。
    Error(String),
    /// 历史被破坏性操作修改（删除/回滚/清理）后的完整快照。
    ///
    /// 仅在服务端执行 `pop_last` / `clean_unclosed` / `rollback_to` / `clear`
    /// 后触发——这些操作不产生其他事件，GUI 无法感知。收到后应用快照
    /// 校准自己的 messages（保护正在 streaming 的消息）。
    ///
    /// `append` 类操作（push_user/assistant/tool）**不**触发此事件——
    /// GUI 已通过 TextDelta / ToolCallStart 等已有事件实时更新。
    HistoryUpdated {
        /// 修改后的完整消息快照。
        messages: Vec<Message>,
    },
}

/// 事件订阅 ID，由 [`crate::chat::ChatService::on_chat`] 返回，
/// 用于 [`crate::chat::ChatService::unsubscribe`] 退订。
///
/// 也用于 [`SubscriptionGuard::id`] 返回值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub(crate) u64);

/// RAII 风格的事件订阅守卫：`Drop` 时自动退订。
///
/// 由 [`crate::chat::ChatService::on_chat_with_guard`] 返回。
/// 推荐把 guard 存放在与业务状态同寿命的作用域（如 Dioxus 页面局部 Signal），
/// 当作用域被 drop（页面切换 / 组件卸载 / 服务端请求结束等）时 guard 自动
/// 退订，从源头避免事件 handler 闭包 + 其捕获的上下文泄漏到已死的生命周期。
///
/// # 设计要点
///
/// - **`Weak<SubscribersInner>`**：guard 不持有 `Arc<Subscribers>`，
///   不延长 service 的寿命；当 service 全部 drop 后 guard 的 Drop 自动
///   成为 no-op（`Weak::upgrade` 返回 `None`）。
/// - **`Send + Sync`**：guard 自身只含 `Weak` + `SubscriptionId`（`Copy`），
///   可在多线程 / 异步任务间自由移动；可与 `ChatSignals` 等 `Copy` 结构共存。
/// - **idempotent Drop**：多次 Drop 安全（重复退订为 no-op）。
///
/// # 与裸 [`SubscriptionId`] 的取舍
///
/// - 拿裸 `SubscriptionId` → 需要调用方自己保证 `unsubscribe(id)`；易遗漏。
/// - 拿 `SubscriptionGuard` → 编译器替你看住生命周期；推荐默认使用。
///
/// 两种 API 并存：老代码可继续用 `on_chat` + `unsubscribe(id)`；新代码用
/// `on_chat_with_guard`。
#[derive(Debug)]
pub struct SubscriptionGuard {
    // 字段 `pub(crate)`：`pub struct` 若字段默认私有，rustc 会把 struct 整体
    // 视为对外不可见（外部无法构造、无法访问）。本 guard 不需要 crate 外访问
    // 字段，`pub(crate)` 即可——同时让 service 子模块的 `detach` / `Drop` 等
    // 操作能正常编译。
    pub(crate) inner: Weak<SubscribersInner>,
    pub(crate) id: SubscriptionId,
}

impl SubscriptionGuard {
    /// 由 [`crate::chat::ChatService::on_chat_with_guard`] 内部调用，
    /// crate 外不可构造（持有 `Weak<SubscribersInner>`）。
    pub(crate) fn new(inner: Weak<SubscribersInner>, id: SubscriptionId) -> Self {
        Self { inner, id }
    }

    /// 返回对应的 [`SubscriptionId`]（高级用法：手动提前退订后仍想保留 guard）。
    pub fn id(&self) -> SubscriptionId {
        self.id
    }

    /// 主动提前退订并消费 guard（之后 Drop 不再做动作）。
    pub fn detach(mut self) {
        self.detach_inner();
        // 把 id 置零，Drop 仍然幂等
        self.id = SubscriptionId(0);
    }

    fn detach_inner(&self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.subs.lock().unwrap().retain(|(sid, _)| *sid != self.id);
        }
        // Weak::upgrade 失败 → service 已全 drop，无需退订
    }
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        self.detach_inner();
    }
}
