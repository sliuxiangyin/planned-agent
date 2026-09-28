//! 运行环境 Context：宿主事实（操作系统 / shell / 可用命令）在 GUI 侧的载体。
//!
//! 数据源是 [`RuntimeEnvironment`]（`planned_agent_core::host`）。**core 只产出结构体**，
//! 渲染成文本、以及「该用什么命令」的策略都属于使用方 —— 见 `crates/core/AGENTS.md`。
//!
//! 设计见 `docs/planned-agent/gui-host-environment.md`。三条使用约定：
//!
//! 1. **取值一律走 [`EnvironmentContext::snapshot`]** —— 它每次都读信号当下值；
//!    不要把结果 clone 进组件 state 长期持有，那会变成过期快照。
//! 2. **刷新只影响下一次执行**：进行中的执行持有的是启动那一刻的快照，
//!    中途换环境会让上下文与已发生的步骤自相矛盾。
//! 3. **可执行环境会「演进」**：首帧是 [`RuntimeEnvironment::detect_host`] 的同步结果
//!    （只有宿主事实），后台探测完成后才补上「哪些命令可用 + 版本」。
//!    用 [`EnvironmentContext::is_probing`] 表达这个中间态。

use dioxus::prelude::*;
use planned_agent_core::host::RuntimeEnvironment;

/// 运行环境上下文（在 `app()` 注入一次，全树共享）。
///
/// 内含 `Signal` 故为 `Copy`：可自由 `move` 进闭包 / 传参，无需 `Arc`。
#[derive(Clone, Copy)]
pub struct EnvironmentContext {
    /// 当前环境。`SyncStorage` 让探测线程能跨线程写回（与 `session.rs` 同一理由）。
    current: Signal<RuntimeEnvironment, SyncStorage>,
    /// 是否正在探测：① 表达中间态 ② 作重入守卫（连点刷新不会并发探测）。
    probing: Signal<bool, SyncStorage>,
}

impl EnvironmentContext {
    /// 创建并**立即启动一次后台探测**（在 `app()` 里调用一次）。
    ///
    /// 初值用 [`RuntimeEnvironment::detect_host`]（同步、零 spawn）播种，
    /// 避免「先空一帧再补上」的闪烁 —— 与 `use_run_subscription` 同一做法。
    pub fn new() -> Self {
        let current = use_signal_sync(RuntimeEnvironment::detect_host);
        let probing = use_signal_sync(|| false);
        let ctx = Self { current, probing };
        ctx.spawn_probe();
        ctx
    }

    /// 取**当下**最新环境。每次要用之前都调它，不要缓存结果。
    pub fn snapshot(&self) -> RuntimeEnvironment {
        self.current.read().clone()
    }

    /// 探测是否进行中（首帧至后台探测完成之间为 `true`）。
    pub fn is_probing(&self) -> bool {
        *self.probing.read()
    }

    /// 重新探测（本机新装了命令 / 改了环境变量后调用）。
    ///
    /// 只影响**后续**的执行；不会改变正在跑的会话。
    /// 探测进行中时本调用是空操作（重入守卫）。
    pub fn refresh(&self) {
        self.spawn_probe();
    }

    /// 后台跑一次完整探测并写回。
    fn spawn_probe(&self) {
        // 重入守卫：peek 只取值、不建立响应式依赖。
        if *self.probing.peek() {
            return;
        }
        // Signal 是 Copy：复制出可变副本用于写入（`McpChangeNotifier::bump` 同一写法）。
        let mut current = self.current;
        let mut probing = self.probing;
        probing.set(true);

        // 为什么自己起线程 + runtime，而不是用 dioxus 的 `spawn`：
        // 探测要 spawn 子进程（`tokio::process`），不赌宿主的异步运行时是否启用了
        // tokio 的 process driver（GUI 里没有 `tokio::process` 的先例；`kv.rs` 用到的
        // `spawn_blocking` 只需要 blocking pool，要求比 process 低）；
        // 顺带也不占用 UI 线程 —— 若干命令各可能等满 1.5s 超时。
        // 写回走 `SyncStorage`，跨线程写正是它的用途。
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(%error, "环境探测：创建 tokio runtime 失败");
                    probing.set(false);
                    return;
                }
            };
            let fresh = runtime.block_on(RuntimeEnvironment::detect());
            tracing::info!(
                os = %fresh.os,
                arch = %fresh.arch,
                available = fresh.executables.available.len(),
                missing = fresh.executables.missing.len(),
                "环境探测完成"
            );
            current.set(fresh);
            probing.set(false);
        });
    }
}

/// 取运行环境上下文（`app()` 已注入）。
///
/// 注意上下文的取值约定：拿到句柄后**每次要用时**再 [`EnvironmentContext::snapshot`]，
/// 不要在此处把值取出来长期持有。
pub fn use_environment() -> EnvironmentContext {
    use_context::<EnvironmentContext>()
}
