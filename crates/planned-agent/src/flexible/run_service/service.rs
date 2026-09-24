//! 宿主门面：命令入队 + 同步查询 + 订阅登记 / 注销。
//!
//! 这是 GUI（以及将来的 CLI）唯一需要持有的句柄：`Arc<RunService>`。

use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use super::store::RunStore;
use super::types::{
    RunCommand, RunRequest, RunSnapshot, RunUpdate, SessionFilter, SubscriptionId,
};

/// 灵活计划执行服务句柄。
pub struct RunService {
    tx: UnboundedSender<RunCommand>,
    store: Arc<RunStore>,
}

impl RunService {
    pub(crate) fn new(tx: UnboundedSender<RunCommand>, store: Arc<RunStore>) -> Self {
        Self { tx, store }
    }

    /// 启动一次执行。
    ///
    /// 输入是一次执行的**完整描述**（[`RunRequest`]）：模板 / 参数 / AI 客户端 / 执行
    /// 配置由调用方给齐（内核零接缝）。服务只负责「同一会话不并发」——
    /// 已在跑则忽略并记日志，**不改动进行中的快照**。
    ///
    /// 没有返回值：受理成功就再无「启动期」状态可报，一切进度与结果都从
    /// [`Self::snapshot`] 或订阅流读，不存在第二条状态通道。
    pub fn start(&self, request: RunRequest) {
        if self.tx.send(RunCommand::Start { request }).is_err() {
            tracing::warn!("灵活执行服务未在运行，启动请求被丢弃");
        }
    }

    /// 请求停止（中断）某会话的执行；返回是否确实发出了取消。
    ///
    /// 同步完成：撤销的是服务内的取消通道，不经过命令队列，
    /// 因此「点停止」立刻生效，不受执行任务当前状态影响。
    pub fn stop(&self, session_id: &str) -> bool {
        self.store.cancel(session_id)
    }

    /// 按会话查当前进度状态；该会话从未执行过 → `None`。
    pub fn snapshot(&self, session_id: &str) -> Option<RunSnapshot> {
        self.store.snapshot(session_id)
    }

    /// 全部会话的当前状态（按开始时间升序）。
    pub fn snapshots(&self) -> Vec<RunSnapshot> {
        self.store.snapshots()
    }

    /// 注册订阅；`sink` 收到的第一条是**当前快照回放**（若已有）。
    ///
    /// 返回的句柄交给 [`Self::unsubscribe`]；宿主组件卸载时应注销，
    /// 注销返回后不会再收到该订阅者的推送。
    pub fn subscribe(
        &self,
        filter: SessionFilter,
        sink: UnboundedSender<RunUpdate>,
    ) -> SubscriptionId {
        self.store.subscribe(filter, sink)
    }

    /// 注销订阅；返回该句柄此前是否存在。
    pub fn unsubscribe(&self, id: SubscriptionId) -> bool {
        self.store.unsubscribe(id)
    }
}
