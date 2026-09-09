mod boot;
mod cache;
mod components;
mod config;
mod context;
mod pages;
mod services;
mod shared;
mod storage;

use boot::{ReadyServices, bootstrap};
use config::GuiConfig;
use context::McpChangeNotifier;
use shared::{BootPhase, BootReporter};
use dioxus::{desktop::Config, prelude::*};
use pages::home::{HomePage, PageRoute};
use pages::plan::PlanPage;
use pages::settings::SettingsPage;
use std::sync::{Arc, OnceLock};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// 全局启动门的就绪载荷特化：`BootPhase` 泛型化后，全局 bootstrap 用 `ReadyServices`。
type AppBootPhase = BootPhase<ReadyServices>;

/// `ReadyServices` 的组件 prop 包装：Dioxus 组件 prop 需 `PartialEq`，
/// 而 `Arc<ReadyServices>` 本身不实现，故用恒等比较的轻量包装。
struct BootServices(Arc<ReadyServices>);
impl Clone for BootServices {
    fn clone(&self) -> Self {
        BootServices(self.0.clone())
    }
}
impl PartialEq for BootServices {
    fn eq(&self, _other: &Self) -> bool {
        true // 就绪后不重建，恒视为等价
    }
}

/// 全局配置实例（main 中初始化一次，app 中通过 Context 消费）
static APP_CONFIG: OnceLock<GuiConfig> = OnceLock::new();

/// 日志文件写入器 guard：必须常驻进程生命周期，否则非阻塞写入会被 drop 后丢失。
/// `OnceLock` 保证只初始化一次；故意 leak，保持静态生命周期。
static LOG_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

const RESET_CSS: Asset = asset!("/assets/reset.css");
const THEME_CSS: Asset = asset!("/assets/dx-components-theme.css");

const BOOT_CSS: Asset = asset!("/assets/boot.css");

fn main() {
    init_logging();

    // 加载配置（失败时自动降级为默认配置）
    let config = GuiConfig::load();
    let _ = APP_CONFIG.set(config);

    dioxus::LaunchBuilder::new()
        .with_cfg(Config::default().with_menu(None))
        .launch(app);
}

/// 初始化日志：仅写入文件（`logs/gui.log.YYYY-MM-DD`，按天轮转），不输出到 CLI/stdout。
fn init_logging() {
    let log_dir = "logs";
    let _ = std::fs::create_dir_all(log_dir);

    let file_appender = tracing_appender::rolling::daily(log_dir, "gui.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("debug"))
        .add_directive("html5ever=warn".parse().expect("valid directive"))
        .add_directive("markup5ever=warn".parse().expect("valid directive"))
        .add_directive("scraper=warn".parse().expect("valid directive"))
        .add_directive("selectors=warn".parse().expect("valid directive"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(file_writer)
        .with_ansi(false)
        .init();

    let _ = LOG_GUARD.set(guard);
}

fn app() -> Element {
    // ── 全局配置（与启动无关，顶层注入） ──
    let config = use_signal(|| APP_CONFIG.get().cloned().unwrap_or_default());
    use_context_provider(|| config);

    // ── MCP 变更通知器（轻量，与 McpContext 解耦；写入后 bump 让 UI 刷新） ──
    let mcp_change_signal = use_signal(|| 0u64);
    use_context_provider(|| McpChangeNotifier::from_signal(mcp_change_signal));

    rsx! {
        document::Stylesheet { href: RESET_CSS }
        document::Stylesheet { href: THEME_CSS }
        BootGate {}
    }
}

/// 启动门组件：渲染 Splash / 错误页 / 就绪壳，决定何时进入主界面。
#[component]
fn BootGate() -> Element {
    // 状态机：Loading(已完成模块名) → Ready / Failed
    let mut phase = use_signal_sync(|| AppBootPhase::Loading(Vec::new()));
    // 触发重试的计数器：递增后重新跑 bootstrap
    let mut attempt = use_signal_sync(|| 0i32);
    // 幂等守卫：本次 attempt 是否已启动过 bootstrap
    let mut boot_started_for = use_signal_sync(|| -1i32);

    use_effect(move || {
        let n = *attempt.read();
        if *boot_started_for.read() == n {
            return; // 本次 attempt 已启动，避免重复触发
        }
        boot_started_for.set(n);

        let cfg = APP_CONFIG.get().cloned().unwrap_or_default();
        // 进度/结果写回器：把「累积进度 + 写 Ready/Failed」的 signal 样板收口
        // （见 `BootReporter`，与灵活模式会话启动共用同一套逻辑）。
        let reporter = BootReporter::new(phase);
        spawn(async move {
            match bootstrap(&cfg, reporter.on_progress()).await {
                Ok(services) => reporter.finish(services),
                Err(errors) => {
                    tracing::error!("启动失败: {:?}", errors);
                    reporter.fail(errors);
                }
            }
        });
    });

    let boot_phase = phase.read().clone();
    match boot_phase {
        AppBootPhase::Loading(done) => rsx! {
            document::Stylesheet { href: BOOT_CSS }
            BootSplash { done }
        },
        AppBootPhase::Failed(errors) => rsx! {
            document::Stylesheet { href: BOOT_CSS }
            BootError {
                errors,
                on_retry: move |_| {
                    phase.set(AppBootPhase::Loading(Vec::new()));
                    attempt.set(attempt() + 1);
                },
            }
        },
        AppBootPhase::Ready(services) => rsx! {
            ReadyShell { services: BootServices(services) }
        },
    }
}

/// 启动加载动画页
#[component]
fn BootSplash(done: Vec<&'static str>) -> Element {
    let labels = [
        ("ai", "AI 管理器"),
        ("prompt", "Prompt 管理器"),
        ("kv", "KV 缓存"),
        ("mcp", "MCP 管理器"),
        ("tools", "工具注册中心"),
        ("rag", "RAG（可选）"),
        ("storage", "Storage 数据库"),
    ];
    rsx! {
        div { class: "boot-splash",
            div { class: "boot-splash__card",
                div { class: "boot-splash__spinner" }
                h1 { class: "boot-splash__title", "正在初始化系统组件…" }
                div { class: "boot-splash__subtitle", "正在加载服务模块…" }
                ul { class: "boot-splash__list",
                    for (key, label) in labels {
                        li { class: if done.contains(&key) { "boot-splash__item done" } else { "" },
                            if done.contains(&key) {
                                span { class: "boot-splash__check", "✔" }
                            } else {
                                span { class: "boot-splash__dot", "·" }
                            }
                            span { class: "boot-splash__label", "{label}" }
                        }
                    }
                }
            }
        }
    }
}

/// 启动失败错误页
#[component]
fn BootError(errors: Vec<(String, String)>, on_retry: EventHandler) -> Element {
    rsx! {
        div { class: "boot-error",
            div { class: "boot-error__card",
                div { class: "boot-error__icon", "⚠" }
                h1 { class: "boot-error__title", "启动失败" }
                p { class: "boot-error__desc", "以下组件初始化失败，无法进入系统： " }
                ul {
                    for (module, err) in &errors {
                        li { class: "boot-error__item",
                            strong { "{module}" }
                            span { "：{err}" }
                        }
                    }
                }
                button {
                    class: "boot-error__retry",
                    onclick: move |_| on_retry.call(()),
                    "重新加载"
                }
            }
        }
    }
}

/// 就绪后唯一渲染的壳：同步注入全部 Arc context，再挂路由。
///
/// **注意**：`use_context_provider` 是 hook，调用次数须在一次渲染内稳定，
/// 因此绝不能放进条件分支——本组件仅在 `Ready` 时渲染一次。
#[component]
fn ReadyShell(services: BootServices) -> Element {
    let services = services.0;
    let ai = services.ai.clone();
    let prompt = services.prompt.clone();
    let kv = services.kv.clone();
    let mcp = services.mcp.clone();
    let tools = services.tools.clone();
    let storage = services.storage.clone();

    use_context_provider(move || ai);
    use_context_provider(move || prompt);
    use_context_provider(move || kv);
    use_context_provider(move || mcp);
    use_context_provider(move || tools);
    use_context_provider(move || storage);

    rsx! { AppRouter {} }
}

/// 顶层路由组件：根据 `PageRoute` 切换 HomePage / PlanPage / SettingsPage。
#[component]
fn AppRouter() -> Element {
    let mut page = use_signal(|| PageRoute::Home);

    let mut navigate = move |route: PageRoute| {
        page.set(route);
    };

    rsx! {
        match page.read().clone() {
            PageRoute::Home => rsx! {
                HomePage {
                    on_navigate: move |r: PageRoute| navigate(r),
                }
            },
            PageRoute::Plan(plan_id) => rsx! {
                match plan_id {
                    Some(id) => rsx! {
                        PlanPage {
                            plan_id: id.clone(),
                            on_back: move |_| navigate(PageRoute::Home),
                        }
                    },
                    None => rsx! {
                        div { class: "plan-page",
                            button {
                                onclick: move |_| navigate(PageRoute::Home),
                                "← 返回指挥中心"
                            }
                            "无效的计划 ID，请返回重新创建。"
                        }
                    },
                }
            },
            PageRoute::Settings => rsx! {
                SettingsPage {
                    on_back: move |_| navigate(PageRoute::Home),
                }
            },
        }
    }
}
