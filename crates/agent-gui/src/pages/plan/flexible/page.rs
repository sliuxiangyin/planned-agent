//! 灵活模式主组件：基于 chat 的聊天面板（含子 agent 支持）。
//!
//! 这是纯视图层：所有 signal / 初始化 / 事件逻辑都收在 `use_flexible_controller`
//! （见 `controller.rs`）。本组件只做「读控制器状态 → 渲染对应 UI」。
//!
//! 初始化未完成时渲染占位；完成后把 controller 的状态与方法喂给通用 `ChatPanel`。

use dioxus::prelude::*;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::ChatPanel;
use crate::components::page_header::PageHeader;

use super::controller::use_flexible_controller;

use dioxus_icons::lucide::Trash2;

#[derive(Props, Clone, PartialEq)]
pub struct FlexiblePageProps {
    pub plan_id: String,
}

#[component]
pub fn FlexiblePage(props: FlexiblePageProps) -> Element {
    let ctl = use_flexible_controller(props.plan_id.clone());

    // ChatService 未就绪 → 占位（controller 内异步初始化完成后会自动 re-render）
    let Some(service) = ctl.service() else {
        return rsx! {
            div { class: "p-4 text-muted-foreground", "灵活模式初始化中…" }
        };
    };

    let busy = ctl.is_busy();
    let current_template = ctl.template();
    let template_label =
        crate::components::chat::chat_panel::template_label(&current_template);

    rsx! {
        div { class: "flexible-page",
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
                chat: ctl.chat,
                chat_service: service,
                on_user_action: move |(action, choice, pending)| {
                    ctl.on_user_action(action, choice, pending);
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
}
