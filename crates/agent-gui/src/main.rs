mod components;
mod config;
mod rag;

use components::{ChatPanel, PlanTimeline, TraceViewer, StatusBar};
use config::GuiConfig;
use dioxus::{desktop::Config, prelude::*};
use rag::RagContext;
use std::sync::{Arc, OnceLock};

/// 全局配置实例（main 中初始化一次，app 中通过 Context 消费）
static APP_CONFIG: OnceLock<GuiConfig> = OnceLock::new();

const RESET_CSS: Asset = asset!("/assets/reset.css");

fn main() {
    tracing_subscriber::fmt::init();

    // 加载配置（失败时自动降级为默认配置）
    let config = GuiConfig::load();
    let _ = APP_CONFIG.set(config);

    dioxus::LaunchBuilder::new()
        .with_cfg(Config::default().with_menu(None))
        .launch(app);
}

#[derive(Debug, Clone, PartialEq)]
enum View {
    Chat,
    Plan,
    Trace,
}

fn app() -> Element {
    // 从全局静态中获取配置，并通过 Context 向下传递给所有子组件
    let config = use_signal(|| APP_CONFIG.get().cloned().unwrap_or_default());
    use_context_provider(|| config);

    // RAG 后台异步初始化（不阻塞 UI）
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

    let mut current_view = use_signal(|| View::Chat);

    rsx! {
        document::Stylesheet { href: RESET_CSS }
        div {
            class: "app-container",
            display: "flex",
            padding:0,
            flex_direction: "column",
            height: "100vh",
            font_family: "system-ui, sans-serif",
            background_color: "#1a1a2e",
            color: "#e0e0e0",

            // Top bar
            header {
                display: "flex",
                align_items: "center",
                justify_content: "space-between",
                padding: "12px 20px",
                background_color: "#16213e",
                border_bottom: "1px solid #0f3460",

                h1 {
                    margin: "0",
                    font_size: "20px",
                    color: "#e94560",
                    "Planned Agent"
                }
                span {
                    font_size: "12px",
                    color: "#888",
                    "Desktop GUI v0.1.0"
                }
            }

            // Main area
            div {
                display: "flex",
                flex: "1",
                overflow: "hidden",

                // Left nav
                nav {
                    display: "flex",
                    flex_direction: "column",
                    width: "200px",
                    padding: "16px 0",
                    background_color: "#16213e",
                    border_right: "1px solid #0f3460",

                    NavButton {
                        label: "Chat".to_string(),
                        active: current_view() == View::Chat,
                        onclick: move |_| current_view.set(View::Chat),
                    }
                    NavButton {
                        label: "Plan".to_string(),
                        active: current_view() == View::Plan,
                        onclick: move |_| current_view.set(View::Plan),
                    }
                    NavButton {
                        label: "Trace".to_string(),
                        active: current_view() == View::Trace,
                        onclick: move |_| current_view.set(View::Trace),
                    }
                }

                // Right content area
                main {
                    flex: "1",
                    padding: "20px",
                    overflow: "auto",

                    match current_view() {
                        View::Chat => rsx! { ChatPanel {} },
                        View::Plan => rsx! { PlanTimeline {} },
                        View::Trace => rsx! { TraceViewer {} },
                    }
                }
            }

            // Bottom status bar
            StatusBar {}
        }
    }
}

#[component]
fn NavButton(label: String, active: bool, onclick: EventHandler<MouseEvent>) -> Element {
    let bg = if active { "#0f3460" } else { "transparent" };
    let color = if active { "#e94560" } else { "#ccc" };

    rsx! {
        button {
            display: "block",
            width: "100%",
            padding: "12px 20px",
            text_align: "left",
            border: "none",
            background_color: bg,
            color: color,
            font_size: "14px",
            cursor: "pointer",
            transition: "background-color 0.2s",
            onclick: move |evt| onclick.call(evt),
            "{label}"
        }
    }
}
