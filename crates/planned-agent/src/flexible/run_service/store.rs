//! 服务共享状态：执行状态表（进度） + 订阅登记表 + 取消通道。
//!
//! 三者合一放在这里，是为了让「查询 / 订阅 / 取消」都能**同步**完成
//! （不必绕命令通道），同时保证状态只有一个写入口（服务循环）。
//!
//! 锁顺序恒定 **runs → subs**（[`RunStore::update`] 与 [`RunStore::subscribe`] 都按此顺序取锁），
//! 因此不会死锁；**订阅登记与推送同锁**，故 [`RunStore::unsubscribe`] 返回之后
//! 该订阅者不会再收到任何推送 —— 宿主可据此在组件卸载时安全回收接收端。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;

use super::types::{RunSnapshot, RunUpdate, SessionFilter, SessionId, SubscriptionId};

/// 一个订阅者：会话范围 + 推送出口。
struct Subscription {
    filter: SessionFilter,
    sink: UnboundedSender<RunUpdate>,
}

/// 执行状态表 + 订阅登记表 + 取消通道。
#[derive(Default)]
pub struct RunStore {
    runs: RwLock<HashMap<SessionId, RunSnapshot>>,
    subs: RwLock<HashMap<SubscriptionId, Subscription>>,
    cancels: RwLock<HashMap<SessionId, watch::Sender<bool>>>,
    next_sub_id: AtomicU64,
}

impl RunStore {
    pub fn new() -> Self {
        Self::default()
    }

    // ───────────────────────────── 状态查询 ─────────────────────────────

    /// 按会话查当前进度状态（同步、无副作用）。
    pub fn snapshot(&self, session_id: &str) -> Option<RunSnapshot> {
        self.runs
            .read()
            .expect("runs 锁")
            .get(session_id)
            .cloned()
    }

    /// 全部会话的快照（按开始时间升序；同刻按会话 id 稳定排序）。
    pub fn snapshots(&self) -> Vec<RunSnapshot> {
        let runs = self.runs.read().expect("runs 锁");
        let mut list = runs.values().cloned().collect::<Vec<_>>();
        list.sort_by(|left, right| {
            (left.started_at_ms, &left.session_id).cmp(&(right.started_at_ms, &right.session_id))
        });
        list
    }

    // ───────────────────────────── 状态写入 ─────────────────────────────

    /// 以闭包整体改写某会话的快照，返回改写后的值。
    ///
    /// - 表里没有该会话时闭包收到 `None`；
    /// - 闭包返回 `None` 表示**删除**该会话的状态（删除不推送）；
    /// - 改写成功后向匹配的订阅者推送，并顺手回收已丢弃接收端的登记。
    pub fn update<F>(&self, session_id: &str, mutate: F) -> Option<RunSnapshot>
    where
        F: FnOnce(Option<RunSnapshot>) -> Option<RunSnapshot>,
    {
        let next = {
            let mut runs = self.runs.write().expect("runs 锁");
            let current = runs.remove(session_id);
            match mutate(current) {
                Some(snapshot) => {
                    runs.insert(session_id.to_string(), snapshot.clone());
                    Some(snapshot)
                }
                None => None,
            }
        };

        let snapshot = next.as_ref()?;
        let update = RunUpdate {
            session_id: session_id.to_string(),
            snapshot: snapshot.clone(),
        };

        let mut stale: Vec<SubscriptionId> = Vec::new();
        {
            let subs = self.subs.read().expect("subs 锁");
            for (id, subscription) in subs.iter() {
                if subscription.filter.matches(session_id)
                    && subscription.sink.send(update.clone()).is_err()
                {
                    // 接收端已被 drop（宿主不再关心）→ 稍后回收登记
                    stale.push(*id);
                }
            }
        }
        if !stale.is_empty() {
            let mut subs = self.subs.write().expect("subs 锁");
            for id in stale {
                subs.remove(&id);
            }
        }
        next
    }

    // ───────────────────────────── 订阅 ─────────────────────────────

    /// 注册订阅，返回注销句柄；**注册后立刻回放**当前匹配的快照。
    ///
    /// 回放与登记在同一把锁内完成（并持有 `runs` 读锁），因此回放的必然是
    /// 「注册那一刻」的表状态，不会被并发写入的旧值覆盖新值。
    pub fn subscribe(
        &self,
        filter: SessionFilter,
        sink: UnboundedSender<RunUpdate>,
    ) -> SubscriptionId {
        let id = self.next_sub_id.fetch_add(1, Ordering::Relaxed) + 1;

        let runs = self.runs.read().expect("runs 锁");
        let mut subs = self.subs.write().expect("subs 锁");
        for snapshot in runs.values() {
            if filter.matches(&snapshot.session_id) {
                let _ = sink.send(RunUpdate {
                    session_id: snapshot.session_id.clone(),
                    snapshot: snapshot.clone(),
                });
            }
        }
        subs.insert(id, Subscription { filter, sink });
        id
    }

    /// 注销订阅；返回该句柄此前是否存在（重复注销返回 `false`）。
    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        self.subs.write().expect("subs 锁").remove(&id).is_some()
    }

    /// 当前订阅者数量（诊断 / 测试用）。
    pub fn subscriber_count(&self) -> usize {
        self.subs.read().expect("subs 锁").len()
    }

    // ───────────────────────────── 取消 ─────────────────────────────

    /// 请求取消某会话的执行（「停止」按钮）；该会话没有在跑 → `false`。
    pub fn cancel(&self, session_id: &str) -> bool {
        match self.cancels.read().expect("cancels 锁").get(session_id) {
            Some(tx) => tx.send(true).is_ok(),
            None => false,
        }
    }

    /// 该会话是否已被请求取消（服务循环据此把终态判成 `Cancelled`）。
    pub(crate) fn is_cancelled(&self, session_id: &str) -> bool {
        self.cancels
            .read()
            .expect("cancels 锁")
            .get(session_id)
            .is_some_and(|tx| *tx.borrow())
    }

    /// 登记取消通道（`Start` 时由服务循环调用）。
    pub(crate) fn register_cancel(&self, session_id: &str, tx: watch::Sender<bool>) {
        self.cancels
            .write()
            .expect("cancels 锁")
            .insert(session_id.to_string(), tx);
    }

    /// 撤销取消通道（服务循环在终态调用）。
    pub(crate) fn clear_cancel(&self, session_id: &str) {
        self.cancels
            .write()
            .expect("cancels 锁")
            .remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flexible::template::{FlexiblePlanTemplate, PlanStep};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    fn template() -> FlexiblePlanTemplate {
        FlexiblePlanTemplate {
            output_schema: None,
            task: "t".to_string(),
            inputs: vec![],
            steps: vec![PlanStep {
                result_reference: "#E1".to_string(),
                intent: "i".to_string(),
                expected_output: "o".to_string(),
                dependencies: vec![],
            }],
        }
    }

    fn snapshot(session_id: &str, run_id: u64) -> RunSnapshot {
        RunSnapshot::started(session_id, run_id, &template())
    }

    /// 订阅一个范围，返回句柄与接收端。
    fn watch(
        store: &RunStore,
        filter: SessionFilter,
    ) -> (SubscriptionId, UnboundedReceiver<RunUpdate>) {
        let (tx, rx) = unbounded_channel();
        (store.subscribe(filter, tx), rx)
    }

    #[test]
    fn snapshot_returns_none_for_unknown_session() {
        let store = RunStore::new();
        assert!(store.snapshot("nope").is_none());
        assert!(store.snapshots().is_empty());
    }

    #[test]
    fn update_inserts_then_deletes() {
        let store = RunStore::new();

        let written = store.update("s1", |_| Some(snapshot("s1", 1)));
        assert_eq!(written.map(|s| s.run_id), Some(1));
        assert_eq!(store.snapshot("s1").map(|s| s.run_id), Some(1));

        // 闭包返回 None = 删除
        let removed = store.update("s1", |current| {
            assert!(current.is_some(), "闭包应收到现值");
            None
        });
        assert!(removed.is_none());
        assert!(store.snapshot("s1").is_none());
    }

    #[test]
    fn subscribe_replays_current_snapshot_immediately() {
        let store = RunStore::new();
        store.update("s1", |_| Some(snapshot("s1", 3)));

        let (_id, mut rx) = watch(&store, SessionFilter::One("s1".to_string()));

        let replayed = rx.try_recv().expect("注册即应回放一条");
        assert_eq!(replayed.session_id, "s1");
        assert_eq!(replayed.snapshot.run_id, 3);
    }

    #[test]
    fn subscriber_only_gets_matching_session() {
        let store = RunStore::new();
        let (_id, mut rx) = watch(&store, SessionFilter::One("s2".to_string()));

        store.update("s1", |_| Some(snapshot("s1", 1)));
        assert!(rx.try_recv().is_err(), "别的会话不应串台");

        store.update("s2", |_| Some(snapshot("s2", 1)));
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn all_filter_receives_every_session() {
        let store = RunStore::new();
        let (_id, mut rx) = watch(&store, SessionFilter::All);

        store.update("s1", |_| Some(snapshot("s1", 1)));
        store.update("s2", |_| Some(snapshot("s2", 1)));

        assert_eq!(rx.try_recv().expect("s1").session_id, "s1");
        assert_eq!(rx.try_recv().expect("s2").session_id, "s2");
    }

    #[test]
    fn unsubscribe_stops_delivery() {
        let store = RunStore::new();
        let (id, mut rx) = watch(&store, SessionFilter::All);
        assert_eq!(store.subscriber_count(), 1);

        assert!(store.unsubscribe(id));
        assert!(!store.unsubscribe(id), "重复注销返回 false");
        assert_eq!(store.subscriber_count(), 0);

        store.update("s1", |_| Some(snapshot("s1", 1)));
        assert!(rx.try_recv().is_err(), "注销后不应再收到推送");
    }

    #[test]
    fn dropped_receiver_is_reclaimed() {
        let store = RunStore::new();
        let (id, rx) = watch(&store, SessionFilter::All);
        drop(rx);

        store.update("s1", |_| Some(snapshot("s1", 1)));

        assert_eq!(store.subscriber_count(), 0, "推送失败应顺手回收登记");
        assert!(store.unsubscribe(id) == false);
    }

    #[test]
    fn cancel_is_false_without_running_session() {
        let store = RunStore::new();
        assert!(!store.cancel("s1"));
    }

    #[test]
    fn cancel_flips_registered_channel() {
        let store = RunStore::new();
        let (tx, rx) = watch::channel(false);
        store.register_cancel("s1", tx);

        assert!(!store.is_cancelled("s1"));
        assert!(store.cancel("s1"));
        assert!(*rx.borrow(), "接收端应看到取消信号");
        assert!(store.is_cancelled("s1"));

        store.clear_cancel("s1");
        assert!(!store.is_cancelled("s1"));
        assert!(!store.cancel("s1"));
    }
}
