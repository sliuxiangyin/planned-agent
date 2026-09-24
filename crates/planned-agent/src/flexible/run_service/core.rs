//! 服务内核：常驻命令循环 + 事件回送 sink。
//!
//! 内核**不依赖 dioxus、不依赖 storage，也不定义任何依赖接缝**：可执行所需的一切
//! （模板 / 参数 / AI 客户端 / 执行配置）都由调用方随 [`RunRequest`] 一起给。
//! 「会话有没有定稿模板」「有没有配 AI 客户端」是宿主的问题，宿主解析完再递进来
//! （见 `docs/planned-agent/flexible-run-service.md` §12）。
//!
//! 执行任务由循环内的 `FuturesUnordered` 轮询（**不额外 `tokio::spawn`**），
//! 因此内核只要求宿主的异步运行时能驱动 [`RunServiceCore::run`] 这一个 future。

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt};
use futures::FutureExt;
use planned_agent_tool_manager::ToolRegistry;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;

use tracing::Instrument;

use crate::flexible::event::{PlanRunEvent, PlanRunSink};
use crate::flexible::executor::FlexibleExecutor;

use super::state::apply_event;
use super::store::RunStore;
use super::types::{RunCommand, RunRequest, RunSnapshot, RunStatus, SessionId};

/// 执行任务的 future：由服务循环轮询。
type RunTask = Pin<Box<dyn Future<Output = ()> + Send>>;

/// 执行服务内核：常驻命令循环。
///
/// 宿主只需 `spawn(core.run())` 一次；`run` 直到命令通道关闭才返回。
pub struct RunServiceCore {
    rx: UnboundedReceiver<RunCommand>,
    inner: RunLoop,
}

impl RunServiceCore {
    pub(crate) fn new(
        store: Arc<RunStore>,
        tools: Arc<ToolRegistry>,
        tx: UnboundedSender<RunCommand>,
        rx: UnboundedReceiver<RunCommand>,
    ) -> Self {
        Self {
            rx,
            inner: RunLoop {
                store,
                tools,
                tx,
                seq: 0,
            },
        }
    }

    /// 常驻循环：收命令 / 推进执行任务。
    ///
    /// 只有命令通道关闭才会返回；宿主持有 [`RunService`](super::RunService)（内含发送端）
    /// 期间频道始终开着，因此服务实际常驻到进程结束 ——
    /// 这正是「切页面不中断执行」的前提。
    pub async fn run(self) {
        let RunServiceCore { mut rx, mut inner } = self;
        let mut tasks: FuturesUnordered<RunTask> = FuturesUnordered::new();

        loop {
            tokio::select! {
                command = rx.recv() => {
                    let Some(command) = command else { break };
                    inner.handle(command, &mut tasks);
                }
                Some(()) = tasks.next(), if !tasks.is_empty() => {}
            }
        }

        tracing::debug!("灵活执行服务退出（命令通道已关闭）");
    }
}

/// 循环内部状态（与 `rx` 分开持有，避免 `select!` 里的可变借用冲突）。
struct RunLoop {
    store: Arc<RunStore>,
    /// 工具可见范围：服务的**长期环境**，与每次执行的输入分开（见设计稿 §12.1）。
    tools: Arc<ToolRegistry>,
    tx: UnboundedSender<RunCommand>,
    /// 执行序号：每次 `start` 递增，作为 `run_id`。
    seq: u64,
}

impl RunLoop {
    fn handle(&mut self, command: RunCommand, tasks: &mut FuturesUnordered<RunTask>) {
        match command {
            RunCommand::Start { request } => self.start(request, tasks),
            RunCommand::Event {
                session_id,
                run_id,
                event,
            } => self.on_event(&session_id, run_id, event),
        }
    }

    /// 处理 `Start`：登记取消 + 写起跑快照（全是同步操作）→ 派任务跑执行器。
    ///
    /// 全程不 await：命令进来的那一刻状态就是确定的（没有「已受理但还没起跑」的中间态），
    /// 因此重复 / 并发的 `Start` 天然被下面的重入检查挡掉。
    fn start(&mut self, request: RunRequest, tasks: &mut FuturesUnordered<RunTask>) {
        let RunRequest {
            session_id,
            template,
            params,
            client,
            config,
        } = request;

        // 同一会话已在跑 → 拒绝（不同会话可并发）。
        // 这里**不动快照**：进行中的进度由在跑的任务继续维护，覆盖反而会把展示搞坏。
        if self
            .store
            .snapshot(&session_id)
            .is_some_and(|snapshot| snapshot.is_running())
        {
            tracing::warn!("会话 {} 正在执行，忽略重复启动", session_id);
            return;
        }

        let run_id = self.next_run_id();
        // 先登记取消通道、再写快照：`stop` 由 GUI 线程直连 `RunStore`（不经过本循环），
        // 两步之间的瞬间点停止会落空。
        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.store.register_cancel(&session_id, cancel_tx);
        self.store.update(&session_id, |_| {
            Some(RunSnapshot::started(session_id.clone(), run_id, &template))
        });
        tracing::info!(
            session = %session_id,
            run_id,
            steps = template.steps.len(),
            provider = %client.provider_name(),
            model = %client.model_name(),
            "受理执行请求"
        );

        let sink = ServiceSink {
            tx: self.tx.clone(),
            session_id: session_id.clone(),
            run_id,
        };
        let store = self.store.clone();
        let tx = self.tx.clone();
        let tools = self.tools.clone();
        // 整个执行任务包在 span 里：executor / step 与它们内部调用的底层 crate（ai-openai、
        // tool-manager）的日志都会带上这个前缀，多会话并发时能分清是谁的日志。
        let span = tracing::info_span!("flexible_run", session = %session_id, run_id);
        let run = async move {
            let executor = FlexibleExecutor::new(client, tools, config);
            // `catch_unwind`：执行任务与常驻循环在**同一个 future** 里被轮询，任务 panic 会
            // unwind 穿过 `core.run` 把整个服务带走 —— 那时所有会话都停了，而 `start` 依旧
            // 返回成功，是最难排查的一种死法。这里把它收敛成「本次执行失败」。
            let outcome =
                AssertUnwindSafe(executor.run(&template, &params, &sink, Some(cancel_rx)))
                    .catch_unwind()
                    .await;

            let reason = match outcome {
                // 正常收尾（含「单步失败」——它只进报告、不以 Err 退出）：终态事件已由 sink 回送。
                Ok(Ok(_report)) => return,
                Ok(Err(error)) => {
                    tracing::error!("会话执行器异常退出: {error:#}");
                    format!("执行器异常退出：{error}")
                }
                Err(_panic) => {
                    tracing::error!("会话执行器 panic（已拦截，服务继续运行）");
                    "执行器内部 panic".to_string()
                }
            };

            // 兜底：若异常 / panic 退出时连终态事件都没发出来，快照会永远停在 `Running`，
            // 后续 `Start` 全被重入检查挡掉 —— 等于把这个会话锁死。补一条失败事件收敛。
            let still_running = store
                .snapshot(&session_id)
                .is_some_and(|snapshot| snapshot.is_running() && snapshot.run_id == run_id);
            if still_running {
                let _ = tx.send(RunCommand::Event {
                    session_id: session_id.clone(),
                    run_id,
                    event: PlanRunEvent::Failed {
                        index: None,
                        error: reason,
                    },
                });
            }
        };
        tasks.push(Box::pin(run.instrument(span)));
    }

    /// 处理执行事件：按 `run_id` 丢弃过期事件后并入快照；终态时收尾。
    fn on_event(&mut self, session_id: &str, run_id: u64, event: PlanRunEvent) {
        let terminal = matches!(
            event,
            PlanRunEvent::RunFinished { .. } | PlanRunEvent::Failed { index: None, .. }
        );
        // 必须在清取消通道**之前**取：`stop` 是先 `send(true)`，终态才清。
        let cancelled = self.store.is_cancelled(session_id);

        let accepted = self
            .store
            .update(session_id, move |current| {
                let mut snapshot = current?; // 没有快照（已被删除）→ 丢弃
                if snapshot.run_id != run_id {
                    // 上一轮任务的迟到事件：原样写回。不能返回 `None`（那等于删除该会话状态），
                    // 所以会换来一条无变化的冗余推送 —— 借此也能让 `accepted` 据 `run_id`
                    // 判断「本轮事件是否真被采纳」。
                    return Some(snapshot);
                }
                apply_event(&mut snapshot, event);
                // 取消标记只用于「本来就没成功」的终态：执行已成功收尾（用户按停止晚了半拍）时
                // 不该把 `Succeeded` 改写成「已取消」。
                if terminal && cancelled && snapshot.status != RunStatus::Succeeded {
                    snapshot.status = RunStatus::Cancelled;
                }
                Some(snapshot)
            })
            .is_some_and(|snapshot| snapshot.run_id == run_id);

        // 只在本轮的事件真的被采纳时收摊：否则一条迟到事件会把**新一轮**的取消通道清掉，
        // 让它的「停止」按钮失效。
        if terminal && accepted {
            self.store.clear_cancel(session_id);
            if let Some(snapshot) = self.store.snapshot(session_id) {
                tracing::info!(
                    session = %session_id,
                    run_id,
                    status = ?snapshot.status,
                    error = snapshot.error.as_deref().unwrap_or("-"),
                    "执行到达终态"
                );
            }
        }
    }

    fn next_run_id(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }
}

/// 执行器事件 → 命令通道的回送端。
struct ServiceSink {
    tx: UnboundedSender<RunCommand>,
    session_id: SessionId,
    run_id: u64,
}

impl PlanRunSink for ServiceSink {
    fn emit(&self, event: PlanRunEvent) {
        // 服务已退出（接收端 drop）→ 静默丢弃：进度展示不该拖垮执行本身。
        let _ = self.tx.send(RunCommand::Event {
            session_id: self.session_id.clone(),
            run_id: self.run_id,
            event,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use async_trait::async_trait;
    use planned_agent_core::ai::types::{ChatCompletionRequest, ChatCompletionResponse};
    use planned_agent_core::ai::{AiClient, ChatCompletionStream};
    use tokio::sync::mpsc::unbounded_channel;

    use crate::flexible::executor::ExecutorConfig;
    use crate::flexible::params::PlanRunParams;
    use crate::flexible::report::{PlanRunReport, StepStatus};
    use crate::flexible::template::{FlexiblePlanTemplate, PlanStep};
    use crate::flexible::testing::{text_response, FakeAiClient};

    use super::super::types::{RunUpdate, SessionFilter, StepPhase, StepTrackLine, SubscriptionId};
    use super::super::RunService;

    // ───────────────────────────── 测试桩 ─────────────────────────────

    /// 给假客户端加延时：让「执行中取消 / 重复启动」成为确定性可测的时序。
    struct SlowAi {
        inner: Arc<FakeAiClient>,
        delay: Duration,
    }

    #[async_trait]
    impl AiClient for SlowAi {
        async fn chat_completion(
            &self,
            request: ChatCompletionRequest,
        ) -> anyhow::Result<ChatCompletionResponse> {
            tokio::time::sleep(self.delay).await;
            self.inner.chat_completion(request).await
        }

        async fn chat_completion_stream(
            &self,
            request: ChatCompletionRequest,
        ) -> anyhow::Result<ChatCompletionStream> {
            self.inner.chat_completion_stream(request).await
        }

        fn provider_name(&self) -> &str {
            "slow-fake"
        }

        fn model_name(&self) -> &str {
            "slow-fake"
        }

        fn default_config(&self) -> ChatCompletionRequest {
            self.inner.default_config()
        }
    }

    // ───────────────────────────── 夹具 ─────────────────────────────

    /// 两步模板：第二步依赖第一步。
    fn template() -> FlexiblePlanTemplate {
        FlexiblePlanTemplate {
            output_schema: None,
            task: "维护文件".to_string(),
            inputs: vec![],
            steps: vec![
                PlanStep {
                    result_reference: "#E1".to_string(),
                    intent: "读取".to_string(),
                    expected_output: "内容".to_string(),
                    dependencies: vec![],
                },
                PlanStep {
                    result_reference: "#E2".to_string(),
                    intent: "追加".to_string(),
                    expected_output: "完成".to_string(),
                    dependencies: vec!["#E1".to_string()],
                },
            ],
        }
    }

    /// 一次自包含的启动请求（模板 + 默认参数 + 客户端 + 默认配置）。
    fn request(session_id: &str, client: Arc<dyn AiClient>) -> RunRequest {
        let template = template();
        let params = PlanRunParams::from_template(&template);
        RunRequest {
            session_id: session_id.to_string(),
            template,
            params,
            client,
            config: ExecutorConfig::default(),
        }
    }

    /// 起一个常驻服务（内核跑到 tokio 任务上），返回门面与状态表。
    ///
    /// 服务本身不持有客户端（客户端随每次请求走），故这里无需任何桩。
    fn spawn_service() -> (Arc<RunService>, Arc<super::RunStore>) {
        let store = Arc::new(super::RunStore::new());
        let (service, core) =
            super::super::new_run_service(store.clone(), Arc::new(ToolRegistry::new()));
        tokio::spawn(core.run());
        (service, store)
    }

    fn subscribe(store: &super::RunStore, session_id: &str) -> UnboundedReceiver<RunUpdate> {
        let (tx, rx) = unbounded_channel();
        let _: SubscriptionId = store.subscribe(SessionFilter::One(session_id.to_string()), tx);
        rx
    }

    /// 收到终态快照（内部超时，避免测试挂死）。
    async fn wait_until_finished(rx: &mut UnboundedReceiver<RunUpdate>) -> RunSnapshot {
        loop {
            let update = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("等待终态超时")
                .expect("服务通道关闭");
            if update.snapshot.status.is_finished() {
                return update.snapshot;
            }
        }
    }

    // ───────────────────────────── 用例 ─────────────────────────────

    /// 正常跑完 → 终态成功 + 报告 + 可同步查询。
    #[tokio::test]
    async fn start_runs_to_success() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 10, 1),
            text_response("第二步产出", 20, 2),
        ]);
        let (service, store) = spawn_service();
        let mut rx = subscribe(&store, "s1");

        service.start(request("s1", ai.clone()));
        let snapshot = wait_until_finished(&mut rx).await;

        assert_eq!(snapshot.status, RunStatus::Succeeded);
        assert_eq!(snapshot.steps.len(), 2);
        assert!(snapshot
            .steps
            .iter()
            .all(|step| step.phase == StepPhase::Done));
        assert_eq!(snapshot.progressed(), 2);
        let report: &PlanRunReport = snapshot.report.as_ref().expect("终态应带报告");
        assert!(report.success);
        assert_eq!(report.prompt_tokens, 30);
        assert_eq!(ai.requests().len(), 2);

        // 需求 3：同步查询与订阅看到同一状态
        let queried = service.snapshot("s1").expect("服务里应有该会话状态");
        assert_eq!(queried.status, RunStatus::Succeeded);
        assert_eq!(queried, snapshot);
    }

    /// 进度是逐事件推进的：中途能看到 current_step 1 → 2。
    #[tokio::test]
    async fn progress_moves_step_by_step() {
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 1, 1),
            text_response("第二步产出", 1, 1),
        ]);
        let (service, store) = spawn_service();
        let mut rx = subscribe(&store, "s1");

        service.start(request("s1", ai.clone()));

        let mut seen_steps: Vec<usize> = Vec::new();
        let snapshot = loop {
            let update = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("超时")
                .expect("关闭");
            if let Some(index) = update.snapshot.current_step {
                seen_steps.push(index);
            }
            if update.snapshot.status.is_finished() {
                break update.snapshot;
            }
        };

        assert!(seen_steps.contains(&1), "应看到第一步开始: {seen_steps:?}");
        assert!(seen_steps.contains(&2), "应看到第二步开始: {seen_steps:?}");
        assert_eq!(snapshot.current_step, Some(2));
    }

    /// 停止：终态 `Cancelled`，未跑的步骤 `Skipped`。
    #[tokio::test]
    async fn stop_marks_cancelled_and_skips_remaining() {
        let inner = FakeAiClient::new(vec![
            text_response("第一步产出", 1, 1),
            text_response("第二步不该跑", 1, 1),
        ]);
        let slow: Arc<dyn AiClient> = Arc::new(SlowAi {
            inner: inner.clone(),
            delay: Duration::from_millis(120),
        });
        let (service, store) = spawn_service();
        let mut rx = subscribe(&store, "s1");

        service.start(request("s1", slow));

        // 等第一步真的开始执行，再取消
        loop {
            let update = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("超时")
                .expect("关闭");
            if update.snapshot.current_step == Some(1) {
                break;
            }
        }
        assert!(service.stop("s1"), "在跑 → 应能发出取消");

        let snapshot = wait_until_finished(&mut rx).await;
        assert_eq!(snapshot.status, RunStatus::Cancelled);
        assert_eq!(snapshot.steps[1].phase, StepPhase::Skipped);
    }

    /// 同一会话重复启动被忽略：只跑一轮。
    #[tokio::test]
    async fn duplicate_start_on_running_session_is_ignored() {
        let inner = FakeAiClient::new(vec![
            text_response("第一步产出", 1, 1),
            text_response("第二步产出", 1, 1),
        ]);
        let slow: Arc<dyn AiClient> = Arc::new(SlowAi {
            inner: inner.clone(),
            delay: Duration::from_millis(80),
        });
        let (service, store) = spawn_service();
        let mut rx = subscribe(&store, "s1");

        service.start(request("s1", slow.clone()));
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("超时")
            .expect("关闭");
        assert!(first.snapshot.is_running());

        // 第二次启动：应被忽略（不起第二个任务）
        service.start(request("s1", slow));

        let snapshot = wait_until_finished(&mut rx).await;
        assert_eq!(snapshot.status, RunStatus::Succeeded);
        assert_eq!(inner.requests().len(), 2, "只应跑一轮（2 次 LLM 调用）");
    }

    /// 上一轮任务的迟到事件按 `run_id` 丢弃。
    #[tokio::test]
    async fn stale_events_are_dropped_by_run_id() {
        let store = Arc::new(super::RunStore::new());
        let ai = FakeAiClient::new(vec![
            text_response("第一步产出", 1, 1),
            text_response("第二步产出", 1, 1),
        ]);
        let (tx, rx) = unbounded_channel();
        let core = RunServiceCore::new(store.clone(), Arc::new(ToolRegistry::new()), tx.clone(), rx);
        tokio::spawn(core.run());
        let service = RunService::new(tx.clone(), store.clone());

        let mut updates = subscribe(&store, "s1");
        service.start(request("s1", ai.clone()));

        // 伪造一条「上一轮」的思考：若未被丢弃，它会被并进该步的轨迹
        tx.send(RunCommand::Event {
            session_id: "s1".to_string(),
            run_id: 999,
            event: PlanRunEvent::StepThought {
                index: 1,
                round: 1,
                text: "陈旧事件".to_string(),
            },
        })
        .expect("发送陈旧事件");

        let snapshot = wait_until_finished(&mut updates).await;
        assert_eq!(snapshot.status, RunStatus::Succeeded);
        let thoughts = snapshot.steps[0]
            .track
            .iter()
            .filter_map(|line| match line {
                StepTrackLine::Thought { text } => Some(text.as_str()),
                StepTrackLine::Tool { .. } => None,
            })
            .collect::<Vec<_>>();
        assert!(
            !thoughts.contains(&"陈旧事件"),
            "旧 run_id 的事件不该被采纳：{thoughts:?}"
        );
    }

    /// 单步失败不结束整次：终态由报告决定，后续步骤 `Skipped`。
    #[tokio::test]
    async fn step_failure_keeps_report_shape() {
        // 只给一个响应：第二步无响应 → 失败
        let ai = FakeAiClient::new(vec![text_response("只有第一步", 1, 1)]);
        let (service, store) = spawn_service();
        let mut rx = subscribe(&store, "s1");

        service.start(request("s1", ai.clone()));
        let snapshot = wait_until_finished(&mut rx).await;

        assert_eq!(snapshot.status, RunStatus::Failed);
        let report: &PlanRunReport = snapshot.report.as_ref().expect("失败也应带回报告");
        assert_eq!(report.steps[0].status, StepStatus::Done);
        assert_eq!(report.steps[1].status, StepStatus::Failed);
        assert_eq!(snapshot.progressed(), 2);
    }

    /// 迟到的旧终态事件不该动**新一轮**的取消通道，否则新一轮的「停止」会失效。
    #[tokio::test]
    async fn stale_terminal_event_keeps_new_cancel_channel() {
        let store = Arc::new(super::RunStore::new());
        let (tx, rx) = unbounded_channel();
        let core = RunServiceCore::new(store.clone(), Arc::new(ToolRegistry::new()), tx.clone(), rx);
        tokio::spawn(core.run());
        let service = RunService::new(tx.clone(), store.clone());
        let mut updates = subscribe(&store, "s1");

        // 第一轮（run_id = 1）：快速跑完并正常收摊
        let fast = FakeAiClient::new(vec![
            text_response("第一步产出", 1, 1),
            text_response("第二步产出", 1, 1),
        ]);
        service.start(request("s1", fast.clone()));
        assert_eq!(
            wait_until_finished(&mut updates).await.status,
            RunStatus::Succeeded
        );

        // 第二轮（run_id = 2）：慢客户端，等它真正开始
        let slow: Arc<dyn AiClient> = Arc::new(SlowAi {
            inner: FakeAiClient::new(vec![
                text_response("第一步产出", 1, 1),
                text_response("第二步产出", 1, 1),
            ]),
            delay: Duration::from_millis(120),
        });
        service.start(request("s1", slow));
        loop {
            let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
                .await
                .expect("超时")
                .expect("关闭");
            if update.snapshot.run_id == 2 && update.snapshot.current_step == Some(1) {
                break;
            }
        }

        // 塞一条「上一轮」的终态事件
        tx.send(RunCommand::Event {
            session_id: "s1".to_string(),
            run_id: 1,
            event: PlanRunEvent::Failed {
                index: None,
                error: "陈旧失败".to_string(),
            },
        })
        .expect("发送陈旧终态事件");
        // 再塞一条**本轮（run_id = 2）**的可观测事件：命令通道是 FIFO，它出现即说明前面那条
        // 陈旧终态已被处理（比用 `sleep` 赌时序确定）。
        tx.send(RunCommand::Event {
            session_id: "s1".to_string(),
            run_id: 2,
            event: PlanRunEvent::StepThought {
                index: 1,
                round: 1,
                text: "同步标记".to_string(),
            },
        })
        .expect("发送同步标记事件");
        loop {
            let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
                .await
                .expect("超时")
                .expect("关闭");
            let has_marker = update.snapshot.steps.iter().any(|step| {
                step.track.iter().any(|line| {
                    matches!(line, StepTrackLine::Thought { text } if text == "同步标记")
                })
            });
            if has_marker {
                break;
            }
        }

        let current = service.snapshot("s1").expect("应有快照");
        assert_eq!(current.run_id, 2);
        assert!(current.is_running(), "陈旧终态不该改坏新一轮: {current:?}");
        assert!(service.stop("s1"), "新一轮的取消通道不该被旧事件清掉");
    }
}
