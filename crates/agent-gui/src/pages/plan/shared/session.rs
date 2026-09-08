//! Plan 页面共享的「会话状态管理中心」。
//!
//! 单一事实源：当前活跃 session id。所有需要感知「当前会话」变化的地方，
//! 都从这里读 / 订阅，而**不认识彼此**：
//! - 异步旁路（step5 落库、flexible_state 定位）：经 tokio watch `receiver().borrow()`
//!   读当前值 —— 天然跟随切换，无需事件。
//! - UI 侧（drawer 高亮、ChatService 宿主将来触发重建）：经 dioxus `current()`
//!   响应式读当前 id —— watch 不会驱动重渲染，故这里补一个 dioxus 信号作为桥。
//!
//! 设计约束（与历史一致）：
//! - 本模块**不认识** `ChatService` / `ChatServiceFactory` / 任何 flexible 类型；
//! - `SessionSlot` 从 `flexible/` 提升为 plan 级共享（原结构即 watch 多订阅者），
//!   仅补 dioxus `current` 信号这一 UI 桥；
//! - `set()` 是唯一写入入口，同时同步 watch 与 dioxus 信号，保证双通道一致。

use std::sync::Arc;

use dioxus::prelude::*;
use tokio::sync::watch;

/// 会话状态管理中心：承载「当前 session id」的 watch + dioxus 双通道。
///
/// 通过 dioxus context 在 plan 页面注入单一实例，controller / drawer / 其它
/// 消费方 `use_context` 取同一个。
pub struct SessionManager {
    /// tokio watch 发送端：异步旁路（step5/state）的当前值来源。
    slot: watch::Sender<Option<String>>,
    /// 常驻接收端：保证 send 永不因无订阅者被丢弃；同时用作同步读取句柄。
    #[allow(dead_code)] // 现阶段仅作保活用途；将来 drawer 列表渲染等经 borrow() 同步读当前值
    value_rx: watch::Receiver<Option<String>>,
    /// dioxus 信号：UI 侧（drawer 高亮等）响应式读当前 id。
    current: Signal<Option<String>, SyncStorage>,
}

impl SessionManager {
    /// 写入当前会话（唯一写入入口）：同步 watch 与 dioxus 信号。
    ///
    /// `session_id` 为 `Some` 表示定位到某会话；`None` 表示无当前会话（清空）。
    pub fn set(&self, session_id: Option<String>) {
        let mut cur = self.current;
        cur.set(session_id.clone());
        let _ = self.slot.send(session_id);
    }

    /// 便捷：定位到某会话。
    pub fn set_active(&self, session_id: String) {
        self.set(Some(session_id));
    }

    /// 清空当前会话（watch 与信号均为 `None`）。
    #[allow(dead_code)] // 待会话抽屉提供「退出当前会话」等动作时使用
    pub fn clear(&self) {
        self.set(None);
    }

    /// 为单个消费方派生 tokio watch receiver（自带当前值快照）。
    ///
    /// 供异步旁路（step5 落库、flexible_state 定位）`borrow()` 读当前值使用。
    pub fn receiver(&self) -> watch::Receiver<Option<String>> {
        self.slot.subscribe()
    }

    /// 同步读取当前会话（watch 侧当前值）。
    #[allow(dead_code)] // 待会话抽屉同步读当前值时使用
    pub fn borrow(&self) -> Option<String> {
        self.value_rx.borrow().clone()
    }

    /// 取 dioxus 响应式句柄（UI 侧读当前 id，变化自动重渲染）。
    #[allow(dead_code)] // 待会话抽屉高亮当前选中项时使用
    pub fn current(&self) -> Signal<Option<String>, SyncStorage> {
        self.current
    }
}

/// 创建 SessionManager（watch 部分持久化于 dioxus hook，跨 render 保持同一实例）。
///
/// 返回值经 `Arc` 共享；组件内应把它作为 context 提供一次，消费方用
/// `use_context::<Arc<SessionManager>>()` 取同一实例，切勿每处自建（否则 watch
/// 各自独立、无法感知同一会话切换）。
pub fn use_session_manager() -> Arc<SessionManager> {
    // dioxus 信号：UI 桥（hook 保证跨 render 稳定同一 handle）
    let current = use_signal_sync(|| None::<String>);
    // watch 通道 + 实例：持久化，仅首次构造
    use_hook(move || {
        let (slot, value_rx) = watch::channel(None::<String>);
        Arc::new(SessionManager {
            slot,
            value_rx,
            current,
        })
    })
}

/// 在组件中创建并把 SessionManager 注入 dioxus context，供整棵子树共享。
pub fn use_provide_session_manager() {
    let mgr = use_session_manager();
    use_context_provider(move || mgr.clone());
}
