//! 灵活计划**执行服务**的 GUI 适配层。
//!
//! 内核（`planned_agent::flexible::run_service`）不依赖 dioxus，也**不定义任何依赖接缝**：
//! 可执行所需的一切（模板 / 参数 / AI 客户端 / 执行配置）都由调用方给齐。因此本模块只做两件事：
//! 1. 在 `ReadyShell` 组装服务并用 `spawn_forever` 启动常驻循环（活过页面与组件卸载）；
//! 2. 宿主侧的「取客户端 → 组请求 → 交给服务」编排（[`start_run_with_template`]）与组件 hooks。
//!
//! 「没有定稿模板」「没有配 AI 客户端」在本层解析成 `Err`，由调用方（UI）呈现；内核只收可执行的
//! 请求。设计见 `docs/planned-agent/flexible-run-service.md` §12。

use std::sync::Arc;

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use planned_agent::flexible::run_service::{
    new_run_service, RunRequest, RunService, RunSnapshot, RunStore, RunUpdate, SessionFilter,
    SubscriptionId,
};
use planned_agent::flexible::{ExecutorConfig, FlexiblePlanTemplate, PlanRunParams};

use crate::boot::ReadyServices;
use crate::context::{require_resource, AiContext};
use crate::services::plans_flexible_service::PlansFlexibleService;

/// 组装并启动执行服务（在 `ReadyShell` 调用一次）。
///
/// 返回的 `plans_flexible_service` 与执行服务**共用同一实例**：页面（读模板 / 定稿回调）
/// 与执行编排（读模板执行）看到的是同一份数据，避免各 new 一个。
///
/// 服务循环用 `spawn_forever` 投到 ROOT scope：切页面、退回首页都不中断。
pub fn start_run_service(services: &ReadyServices) -> (Arc<RunService>, Arc<PlansFlexibleService>) {
    let plans_flexible_service = Arc::new(PlansFlexibleService::new(
        services.storage.plans_flexible_sessions_repo(),
        services.storage.flexible_state_repo(),
    ));

    // 服务只持「长期环境」（状态表 + 工具可见范围）；AI 客户端与执行配置随每次请求走。
    let (run_service, core) = new_run_service(
        Arc::new(RunStore::new()),
        services.tools.registry.clone(),
    );
    spawn_forever(core.run());

    (run_service, plans_flexible_service)
}

/// 宿主侧编排：组 [`RunRequest`] → 交给服务执行。
///
/// 调用方给的是**已在手上的定稿模板**（左面板就是这条路径：模板来自该会话的 `load_template`，
/// 页面早已读到）。唯一可能失败的是取 AI 客户端（没配 provider）。
///
/// 为什么不做成「传 `session_id` 让服务去读库」：那正是 v1 的接缝形态，会把「会话是否定稿 /
/// 模板能否反序列化」带进执行路径（见设计稿 §12）。将来首页要「直接跑某个会话」时，
/// 在调用方 `load_template` 之后再调本函数即可 —— 三态解析留在 UI 层。
pub fn start_run_with_template(
    service: Arc<RunService>,
    ai: Arc<AiContext>,
    session_id: String,
    template: FlexiblePlanTemplate,
    params: PlanRunParams,
) -> Result<(), String> {
    let client = ai
        .manager
        .default()
        .map_err(|error| format!("AI 客户端不可用：{error}"))?;

    service.start(RunRequest {
        session_id,
        template,
        params,
        client,
        config: ExecutorConfig::default(),
    });
    Ok(())
}

/// 取执行服务句柄（`ReadyShell` 已注入）。
pub fn use_run_service() -> Arc<RunService> {
    require_resource::<RunService>()
}

/// 订阅某会话的执行状态。
///
/// - `session_id` 传会话中心的 `SessionManager::current()`：**切换会话自动改订**；
/// - 返回的 Signal 恒为该会话最新快照（订阅时服务会**立刻回放**当前快照，
///   因此已执行过的会话不会出现"订阅了却空白"）；
/// - 组件卸载时自动注销订阅（`use_drop`），不会往已释放的信号上写。
///
/// 首帧用一次同步查询播种（`use_signal_sync` 初值），避免"先空一帧再补上"的闪烁。
pub fn use_run_subscription(
    session_id: Signal<Option<String>, SyncStorage>,
) -> Signal<Option<RunSnapshot>, SyncStorage> {
    let service = use_run_service();
    // 首帧兜底：用 peek 读会话 id（不建立依赖），同步查一次服务里的现有快照。
    let initial = session_id
        .peek()
        .as_deref()
        .and_then(|id| service.snapshot(id));
    let mut snapshot = use_signal_sync(move || initial);
    let mut subscription = use_signal_sync(|| None::<SubscriptionId>);

    // effect 与 drop 都要用服务句柄，各持一份（`Arc` 克隆不贵）。
    let subscribe_service = service.clone();
    use_effect(move || {
        // 读 session_id 即订阅：切换会话时本 effect 重跑，改订新会话。
        let next = session_id.read().clone();

        // peek：只取值、不建立对 `subscription` 的依赖；否则下面的 set
        // 会反过来触发本 effect（自激循环）。
        // 先落到局部变量：`peek()` 的借用守卫活到语句末尾，写进 if-let 条件
        // 会让下面的 `set` 撞上「借用中」错误。
        let previous = *subscription.peek();
        if let Some(previous) = previous {
            subscribe_service.unsubscribe(previous);
            subscription.set(None);
        }

        let Some(session_id) = next else {
            snapshot.set(None);
            return;
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RunUpdate>();
        let id = subscribe_service.subscribe(SessionFilter::One(session_id.clone()), tx);
        subscription.set(Some(id));

        // 换会话必须**重置**本地快照：不清就会把上一个会话的进度画到新会话的模板上（相位错位），
        // 旧会话处于 Running 时还会把新会话的执行按钮一起置灰。
        // 用一次同步查询播种，而不是清成 `None`：订阅回放要等消费任务被 poll 才到，
        // 清空会让「已有历史快照的会话」闪一帧空态。
        // 顺序上**先订阅、后播种**：万一服务那唯一一次推送恰好落在两步之间，通道里已经排队，
        // 消费任务随后会写入；反过来（先查后订）则可能永久停在旧值上。
        snapshot.set(subscribe_service.snapshot(&session_id));

        // 消费推送到组件本地 Signal（任务挂在当前 scope，卸载即被取消）。
        // `session_id` 是本任务「只认这个会话」的凭据：effect 重跑后，**旧任务可能仍会读到
        // 通道里已缓冲的旧消息**（`unsubscribe` 只保证不再 push），必须自己过滤，
        // 否则切会话时两个会话的数据会相互覆盖。
        spawn(async move {
            while let Some(update) = rx.recv().await {
                if update.session_id != session_id {
                    continue;
                }
                snapshot.set(Some(update.snapshot));
            }
        });
    });

    use_drop(move || {
        if let Some(id) = *subscription.peek() {
            service.unsubscribe(id);
        }
    });

    snapshot
}
