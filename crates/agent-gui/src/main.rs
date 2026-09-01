mod cache;
mod components;
mod config;
mod context;
mod pages;
mod services;
mod storage;

use config::GuiConfig;
use context::{
    AiContext, InitStatus, KvContext, McpChangeNotifier, McpContext, PromptContext, RagContext,
    StorageContext, ToolsContext,
};
use context::tools::plans_flexible_tool::register_plans_flexible_tool;
use dioxus::{desktop::Config, prelude::*};
use pages::home::{HomePage, PageRoute};
use pages::plan::PlanPage;
use pages::settings::SettingsPage;
use services::plans_flexible_service::PlansFlexibleService;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// 全局配置实例（main 中初始化一次，app 中通过 Context 消费）
static APP_CONFIG: OnceLock<GuiConfig> = OnceLock::new();

/// 日志文件写入器 guard：必须常驻进程生命周期，否则非阻塞写入会被 drop 后丢失。
/// `OnceLock` 保证只初始化一次；故意 leak，保持静态生命周期。
static LOG_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

const RESET_CSS: Asset = asset!("/assets/reset.css");
const THEME_CSS: Asset = asset!("/assets/dx-components-theme.css");

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
///
/// 文件路径与命名规范与 CLI（`planned-agent`）保持一致：目录 `logs/`，
/// 文件名 `gui.log`（CLI 用 `agent.log`，二者并存不冲突）。
fn init_logging() {
    let log_dir = "logs";
    let _ = std::fs::create_dir_all(log_dir);

    let file_appender = tracing_appender::rolling::daily(log_dir, "gui.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    // 默认 info 级别；强制屏蔽 scraper / html5ever 等 DEBUG 刷屏
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("debug"))
        .add_directive("html5ever=warn".parse().expect("valid directive"))
        .add_directive("markup5ever=warn".parse().expect("valid directive"))
        .add_directive("scraper=warn".parse().expect("valid directive"))
        .add_directive("selectors=warn".parse().expect("valid directive"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(file_writer)
        .with_ansi(false) // 写文件时不需要 ANSI 颜色
        .init();

    // guard 必须常驻（详见 static LOG_GUARD 注释）
    let _ = LOG_GUARD.set(guard);
}

fn app() -> Element {
    // ── 全局配置 ──
    let config = use_signal(|| APP_CONFIG.get().cloned().unwrap_or_default());
    use_context_provider(|| config);

    // ── 6 个独立 Resource（互不依赖，并行 init） ────────────────────────

    // 1. AI 管理器（同步 init）
    let ai: Resource<Option<Arc<AiContext>>> = use_resource(move || {
        let cfg = config.read().ai_providers.clone();
        async move {
            match AiContext::init(&cfg) {
                Ok(ctx) => Some(Arc::new(ctx)),
                Err(e) => {
                    tracing::warn!("AI 不可用: {}", e);
                    None
                }
            }
        }
    });
    use_context_provider(|| ai);

    // 2. Prompt 管理器（异步 init：加载 prompts/ 目录）
    let prompt: Resource<Option<Arc<PromptContext>>> = use_resource(move || {
        let cfg = config.read().prompt_manager.clone();
        async move {
            match PromptContext::init(&cfg).await {
                Ok(ctx) => Some(Arc::new(ctx)),
                Err(e) => {
                    tracing::warn!("Prompt 不可用: {}", e);
                    None
                }
            }
        }
    });
    use_context_provider(|| prompt);

    // 3. KV 缓存（异步 init：spawn_blocking 内打开 sled）
    //    必须在 MCP 之前声明：MCP 的 use_resource 闭包会捕获 `kv`。
    let kv: Resource<Option<Arc<KvContext>>> = use_resource(move || {
        let cache_cfg = config.read().cache.clone();
        async move {
            match KvContext::init(&cache_cfg).await {
                Ok(ctx) => Some(Arc::new(ctx)),
                Err(e) => {
                    tracing::warn!("KV 缓存不可用，应用将无法使用本地 KV 缓存: {}", e);
                    None
                }
            }
        }
    });
    use_context_provider(|| kv);

    // 3.5 MCP 变更通知器（与 McpContext 解耦的轻量 Dioxus context）
    //     任何对 MCP 数据的写入后都 bump()，让 list_page 等 UI 重新加载视图
    let mcp_change_signal = use_signal(|| 0u64);
    use_context_provider(|| McpChangeNotifier::from_signal(mcp_change_signal));

    // 4. MCP 管理器（异步 init：按场景选择 KV / 文件存储，10s 超时）
    let mcp: Resource<Option<Arc<McpContext>>> = use_resource(move || async move {
        // 等 kv Resource 就绪后再 init（mcp 与 kv 并发，mcp 可能先跑）。
        // 最多等 500ms，每 50ms 检查一次；超时则降级到文件存储。
        let kv_arc = wait_for_kv_ready(&kv, Duration::from_millis(500)).await;
        match McpContext::init(kv_arc).await {
            Ok(ctx) => Some(Arc::new(ctx)),
            Err(e) => {
                tracing::warn!("MCP 不可用: {}", e);
                None
            }
        }
    });
    use_context_provider(|| mcp);

    // 5. Tool Registry（同步 init：仅注册内置 provider；MCP 后续延后注入）
    let tools: Resource<Option<Arc<ToolsContext>>> = use_resource(move || {
        let docs_dir = config.read().prompt_manager.prompt_dir.join("docs");
        async move {
            match ToolsContext::init(docs_dir) {
                Ok(ctx) => Some(Arc::new(ctx)),
                Err(e) => {
                    tracing::warn!("Tools 不可用: {}", e);
                    None
                }
            }
        }
    });
    use_context_provider(|| tools);

    // 6. RAG（异步 init：既有逻辑）
    let rag: Resource<Option<Arc<RagContext>>> = use_resource(move || {
        let rag_cfg = config.read().rag.clone();
        async move {
            match RagContext::init(&rag_cfg).await {
                Ok(ctx) => Some(Arc::new(ctx)),
                Err(e) => {
                    tracing::warn!("RAG 不可用: {}", e);
                    None
                }
            }
        }
    });
    use_context_provider(|| rag);

    // 7. Storage（异步 init：打开 SQLite + 跑 migration）
    let storage: Resource<Option<Arc<StorageContext>>> = use_resource(move || {
        let storage_cfg = config.read().storage.clone();
        async move {
            match StorageContext::init(&storage_cfg).await {
                Ok(ctx) => Some(Arc::new(ctx)),
                Err(e) => {
                    tracing::warn!("Storage 不可用，应用将以纯 mock 数据运行: {}", e);
                    None
                }
            }
        }
    });
    use_context_provider(|| storage);

    // 8. KV 缓存（异步 init：spawn_blocking 内打开 sled）—— 已上移到第 3 位供 mcp 引用

    // ── InitStatus 快照（响应式：随 7 个 Resource 变化自动重算） ──
    let init_status = use_memo(move || {
        InitStatus::from_resources(&ai, &prompt, &mcp, &tools, &rag, &storage, &kv)
    });
    use_context_provider(|| init_status);

    // ── 延后注入 MCP → Tools ──
    // use_effect 在 tools / mcp 任一变化时触发：
    // 1. 注册 MCP 缓存工具到 ToolRegistry
    // 2. 注入 McpManager 到 ToolRegistry（用于后续工具调用）
    use_effect(move || {
        let tools_arc = tools.read().as_ref().and_then(|x| x.clone());
        let mcp_arc = mcp.read().as_ref().and_then(|x| x.clone());
        if let (Some(t), Some(m)) = (tools_arc, mcp_arc) {
            // 1. 从缓存注册 MCP 工具（不连接服务器）
            m.register_cached_tools(&t.registry);
            // 2. 注入 McpManager 用于后续工具调用路由
            t.set_mcp_manager(m.manager.clone());
        }
    });

    // ── 延后注册 plans_flexible 自定义工具 ──
    // storage 与 tools 并发初始化；storage 就绪后把 PlansFlexibleService 注入
    // 并注册 `plans_flexible` 工具（executor 需要真实数据源）。
    use_effect(move || {
        let tools_arc = tools.read().as_ref().and_then(|x| x.clone());
        let storage_arc = storage.read().as_ref().and_then(|x| x.clone());
        if let (Some(t), Some(s)) = (tools_arc, storage_arc) {
            let service =
                Arc::new(PlansFlexibleService::new(s.plans_flexible_repo()));
            register_plans_flexible_tool(&t, service);
        }
    });

    rsx! {
        document::Stylesheet { href: RESET_CSS }
        document::Stylesheet { href: THEME_CSS }
        AppRouter {}
    }
}

/// 顶层路由组件：根据 `PageRoute` 切换 HomePage / PlanPage / SettingsPage。
/// MCP 服务作为 `SettingsPage` 内部的嵌套视图（不再走顶级路由）。
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
                        // 不应出现：新建计划现在通过弹窗创建，不再走 plan_id=None 路径
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

/// 轮询等待 KV Resource 就绪（最多等 `max_wait`），返回 `Arc<KvContext>` 或 `None`
///
/// 用途：mcp 与 kv 两个 Resource 并发 init 时，mcp 可能先跑。
/// 这里在调用 [`McpContext::init`] 前先等一会 kv，超时则降级到文件存储。
async fn wait_for_kv_ready(
    kv: &Resource<Option<Arc<KvContext>>>,
    max_wait: Duration,
) -> Option<Arc<KvContext>> {
    let start = Instant::now();
    let interval = Duration::from_millis(50);
    loop {
        if let Some(Some(arc)) = kv.read().as_ref() {
            return Some(arc.clone());
        }
        if start.elapsed() >= max_wait {
            return None;
        }
        tokio::time::sleep(interval).await;
    }
}