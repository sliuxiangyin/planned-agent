mod components;
mod config;
mod context;
mod pages;
mod services;
mod storage;

use config::GuiConfig;
use context::{
    AiContext, InitStatus, McpContext, PromptContext, RagContext, StorageContext, ToolsContext,
};
use dioxus::{desktop::Config, prelude::*};
use pages::home::{HomePage, PageRoute};
use pages::plan::PlanPage;
use pages::settings::SettingsPage;
use std::sync::{Arc, OnceLock};
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
        .unwrap_or_else(|_| EnvFilter::new("info"))
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

    // 3. MCP 管理器（异步 init：从 mcp-config.json 读取配置，连接所有 server，10s 超时）
    let mcp: Resource<Option<Arc<McpContext>>> = use_resource(move || async move {
        match McpContext::init().await {
            Ok(ctx) => Some(Arc::new(ctx)),
            Err(e) => {
                tracing::warn!("MCP 不可用: {}", e);
                None
            }
        }
    });
    use_context_provider(|| mcp);

    // 4. Tool Registry（同步 init：仅注册内置 provider；MCP 后续延后注入）
    let tools: Resource<Option<Arc<ToolsContext>>> = use_resource(move || async move {
        match ToolsContext::init() {
            Ok(ctx) => Some(Arc::new(ctx)),
            Err(e) => {
                tracing::warn!("Tools 不可用: {}", e);
                None
            }
        }
    });
    use_context_provider(|| tools);

    // 5. RAG（异步 init：既有逻辑）
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

    // 6. Storage（异步 init：打开 SQLite + 跑 migration）
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

    // ── InitStatus 快照（响应式：随 6 个 Resource 变化自动重算） ──
    let init_status = use_memo(move || {
        InitStatus::from_resources(&ai, &prompt, &mcp, &tools, &rag, &storage)
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
                PlanPage {
                    plan_id: plan_id.clone(),
                    on_back: move |_| navigate(PageRoute::Home),
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