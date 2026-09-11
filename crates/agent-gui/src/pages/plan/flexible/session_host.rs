//! 灵活模式**单会话常驻宿主**（`FlexibleSessionHost`）。
//!
//! 每个会话一个实例，无论 active 与否都保持挂载：后台会话的 `ChatBridge`（内含
//! `ChatService` / driver）继续收流并落库（保活）；仅 active 时渲染 `ChatPanel`。
//! 非 active 时在读 view signal **之前** early-return，故不订阅 view、后台更新不触发重渲染。
//!
//! 设计见 `docs/chat-flexible-多会话保活设计.md` §2 / §3 A。

use dioxus::prelude::*;
use dioxus_icons::lucide::Trash2;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::ChatPanel;
use crate::components::page_header::PageHeader;

use super::controller::use_flexible_controller;
use super::session_boot::FlexBoot;

#[derive(Props, Clone, PartialEq)]
pub(crate) struct FlexibleSessionHostProps {
    pub plan_id: String,
    pub session_id: String,
    pub active: bool,
    pub templates: Signal<Vec<String>, SyncStorage>,
}

/// 单会话常驻宿主：无论 active 与否都挂载（后台会话继续收流/落库），
/// 仅 active 时渲染 `ChatPanel`。
#[component]
pub(crate) fn FlexibleSessionHost(props: FlexibleSessionHostProps) -> Element {
    // ⚠️ 全部 hooks 必须在任何 early-return 之前调用（后台宿主也要 boot / 保持订阅）。
    let ctl = use_flexible_controller(
        props.plan_id.clone(),
        props.session_id.clone(),
        props.templates,
    );

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
