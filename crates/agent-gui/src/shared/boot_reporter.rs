//! 启动进度/结果写回器：把 `Signal<BootPhase<T>>` 的读写样板收口。

use std::sync::Arc;

use dioxus::prelude::*;

use super::boot_phase::{BootPhase, OnProgress};

/// 启动进度/结果写回器。
///
/// - 方法均 `&self`，内部消化 `Signal::set(&mut self)` 与「读-改-写回」样板，
///   调用方无需关心 `set` 要 `mut`、无需手动拷贝句柄；
/// - 无条件 `Clone`/`Copy`（`Signal` 本就无条件 Copy），可安全 `move` 进闭包多次；
/// - 全局 `bootstrap` 与灵活模式 `boot_flexible_session` 共用同一套进度上报逻辑。
pub struct BootReporter<T: 'static + Send + Sync> {
    signal: Signal<BootPhase<T>, SyncStorage>,
}

impl<T: 'static + Send + Sync> BootReporter<T> {
    pub fn new(signal: Signal<BootPhase<T>, SyncStorage>) -> Self {
        Self { signal }
    }

    /// 生成可喂给 `bootstrap` / `boot_flexible_session` 的进度回调。
    pub fn on_progress(self) -> OnProgress {
        Arc::new(move |name| self.record(name))
    }

    /// 累积一个已完成的阶段名进 `Loading` 列表（忽略重复，就绪/失败后不再回写）。
    fn record(&self, name: &'static str) {
        // 注意：不能把 `set` 写回放进 `if let ... = self.signal.read().clone() { }` 的 body 里——
        // `read()` 返回的 read guard（读借用）会存活到 if-let 结束，此刻再对同一 signal 调 `set`(写锁)
        // 会在同一线程形成「读锁未释放就取写锁」的自死锁（抽离 BootReporter 前写法可避开，回归由此引入）。
        // 因此先把当前 Loading 列表拷贝出来（guard 随语句结束即释放），再独立写回。
        let done = {
            let guard = self.signal.read();
            match &*guard {
                BootPhase::Loading(d) => d.clone(),
                _ => return, // 就绪/失败后不再回写
            }
        };
        if !done.contains(&name) {
            let mut d = done;
            d.push(name);
            let mut s = self.signal;
            s.set(BootPhase::Loading(d));
        }
    }

    /// 写入就绪结果。
    pub fn finish(&self, payload: T) {
        let mut s = self.signal;
        s.set(BootPhase::Ready(Arc::new(payload)));
    }

    /// 写入失败清单。
    pub fn fail(&self, errors: Vec<(String, String)>) {
        let mut s = self.signal;
        s.set(BootPhase::Failed(errors));
    }
}

impl<T: 'static + Send + Sync> Clone for BootReporter<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: 'static + Send + Sync> Copy for BootReporter<T> {}
