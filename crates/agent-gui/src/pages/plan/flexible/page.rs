//! 灵活模式主组件（多会话保活）。
//!
//! 结构：
//! - `FlexiblePage`（壳）：维护「已打开会话集合」，把每个会话渲染成一个常驻
//!   `FlexibleSessionHost`；当前 active 由 `SessionManager` 决定。
//! - `FlexibleSessionHost`（常驻宿主）：每个会话一个，各自 boot 一次并把
//!   `ChatBridge`（内含 `ChatService`）留在组件里 —— 切换只改「谁 active」，
//!   不卸载任何宿主，后台会话的 driver 持续收流/落库（保活）。
//!
//! 设计见 `docs/chat-flexible-多会话保活设计.md` §2 / §3 A。

use std::sync::Arc;

use dioxus::prelude::*;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::ChatPanel;
use crate::components::page_header::PageHeader;
use crate::pages::plan::shared::session::SessionManager;

use super::controller::use_flexible_controller;
use super::session_boot::FlexBoot;
use dioxus_icons::lucide::Trash2;

#[derive(Props, Clone, PartialEq)]
pub struct FlexiblePageProps {
    pub plan_id: String,
    pub session_id: String,
}

/// 灵活模式「壳」：维护已打开会话集合并渲染各会话宿主。
#[component]
pub fn FlexiblePage(props: FlexiblePageProps) -> Element {
    // 会话状态管理中心（PlanPage 已注入 context）：当前 active 会话。
    let session_mgr = use_context::<Arc<SessionManager>>();
    let active = session_mgr.current(); // Signal<Option<String>>

    // 已打开会话集合：首帧含 props.session_id；每出现新 active 就 ensure 加入。
    // （本阶段只增不减；LRU 回收见设计文档 §5 决策 1。）
    // 注：仅用 props.session_id 作初值；调用方保证 plan 就绪后才渲染本组件。
    let open = use_signal_sync({
        let init = props.session_id.clone();
        move || {
            if init.is_empty() {
                Vec::<String>::new()
            } else {
                vec![init]
            }
        }
    });

    let mut open_ensure = open; // Copy 句柄
    let active_listen = active;
    use_effect(move || {
        if let Some(id) = active_listen.read().clone() {
            if !id.is_empty() {
                let mut o = open_ensure.write();
                if !o.contains(&id) {
                    o.push(id);
                }
            }
        }
    });

    // 当前 active 会话 id（无当前会话时回退到 props 初值）。
    let active_id = active
        .read()
        .clone()
        .unwrap_or_else(|| props.session_id.clone());
    let sessions = open.read().clone();

    rsx! {
        div { class: "flexible-page",
            for sid in sessions.iter().cloned() {
                FlexibleSessionHost {
                    key: "{sid}",
                    plan_id: props.plan_id.clone(),
                    session_id: sid.clone(),
                    active: sid == active_id,
                }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct FlexibleSessionHostProps {
    plan_id: String,
    session_id: String,
    active: bool,
}

/// 单会话常驻宿主：无论 active 与否都挂载（后台会话继续收流/落库），
/// 仅 active 时渲染 `ChatPanel`。
#[component]
fn FlexibleSessionHost(props: FlexibleSessionHostProps) -> Element {
    // ⚠️ 全部 hooks 必须在任何 early-return 之前调用（后台宿主也要 boot / 保持订阅）。
    let ctl = use_flexible_controller(props.plan_id.clone(), props.session_id.clone());

    // 非 active：不渲染 ChatPanel（不订阅 view → 后台写 view 不触发重渲染），
    // 但 hooks / 订阅桥照常存活 → 保活。
    if !props.active {
        return rsx! {
            div { class: "flexible-host--background", style: "display: none;" }
        };
    }

    // ChatService 未就绪 → 占位（controller 内异步初始化完成后会自动 re-render）。
    let ready_session = match ctl.boot_phase() {
        FlexBoot::Loading(_) => {
            return rsx! {
                div { class: "p-4 text-muted-foreground", "灵活模式初始化中…" }
            };
        }
        FlexBoot::Failed(errors) => {
            return rsx! {
                div { class: "p-4 text-destructive", "灵活模式初始化失败：" }
                ul {
                    for (module, err) in &errors {
                        li { "{module}: {err}" }
                    }
                }
            };
        }
        FlexBoot::Ready(session) => session,
    };

    let bridge = ready_session.bridge.clone();
    let busy = ctl.is_busy();
    let current_template = ctl.template();
    let template_label = crate::components::chat::chat_panel::template_label(&current_template);

    rsx! {
        PageHeader {
            title: "灵活模式".to_string(),
            class: Some("dx-page-header--nested".to_string()),
            actions: {
                rsx! {
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::IconSm,
                        disabled: busy,
                        title: "清空会话",
                        onclick: move |_| ctl.clear_session(),
                        Trash2 { size: "16" }
                    }
                }
            },
        }

        ChatPanel {
            view: ctl.view,
            input_text: ctl.input_text,
            bridge,
            on_user_action: move |(choice, pending)| {
                ctl.on_user_action(choice, pending);
            },
            template_label: template_label,
            templates: ctl.templates(),
            on_template_change: Some(
                Callback::new(move |name: String| ctl.apply_template(name)),
            ),
            thinking: ctl.thinking(),
            on_thinking_change: Some(
                Callback::new(move |v: bool| ctl.set_thinking(v)),
            ),
            temperature: ctl.temperature(),
            on_temperature_change: Some(
                Callback::new(move |v: String| ctl.set_temperature(v)),
            ),
            on_clear: move |_| ctl.clear_session(),
        }
    }
}
