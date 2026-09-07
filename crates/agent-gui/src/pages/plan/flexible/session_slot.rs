//! 会话 id 共享槽 —— 承载「当前活跃会话 id」的 tokio watch 通道。
//!
//! 从 `controller.rs` 拆出。承载的是「最新值」而非事件流：消费方（如 step5 callback）
//! 随时 `receiver().borrow()` 读当前 session；controller 建/切会话时 `set()`。

use tokio::sync::watch;

/// 共享「当前会话 id」槽（tokio watch）。
///
/// 相比手写 `Arc<RwLock>` 语义更明确，且保留常驻 `_anchor` receiver，保证 `send`
/// 永不因无订阅者而被丢弃（持久语义与旧实现一致）。
pub(super) struct SessionSlot {
    tx: watch::Sender<Option<String>>,
    #[allow(dead_code)] // 常驻保活：保证 send 总被记录；不做读取
    _anchor: watch::Receiver<Option<String>>,
}

impl SessionSlot {
    pub(super) fn new() -> Self {
        let (tx, rx) = watch::channel(None);
        Self { tx, _anchor: rx }
    }
    /// 记录当前会话（此后 subscribe 的消费方可立即读到该最新值）。
    pub(super) fn set(&self, session_id: String) {
        let _ = self.tx.send(Some(session_id));
    }
    /// 为单个消费方派生 receiver（自带当前值快照）。
    pub(super) fn receiver(&self) -> watch::Receiver<Option<String>> {
        self.tx.subscribe()
    }
}
