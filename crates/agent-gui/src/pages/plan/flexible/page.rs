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
use crate::components::sheet::{Sheet, SheetContentClose, SheetHeader, SheetTitle};

use super::controller::use_flexible_controller;
use super::session_boot::FlexBoot;
use dioxus_icons::lucide::{History, Trash2};

use crate::pages::plan::sessions::SessionListSheet;

#[derive(Props, Clone, PartialEq)]
pub struct FlexiblePageProps {
    pub plan_id: String,
}

#[component]
pub fn FlexiblePage(props: FlexiblePageProps) -> Element {
    // 会话抽屉开关：外壳 Sheet 常驻并受控（负责滑出动画/遮罩/关闭）；内容组件
    // SessionListSheet 仅在开启时挂载（挂载即 use_resource 拉取，关闭即卸载取消）。
    let mut sessions_open = use_signal_sync(|| false);
    let ctl = use_flexible_controller(props.plan_id.clone());

    // ChatService 未就绪 → 占位（controller 内异步初始化完成后会自动 re-render）
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

    let service = ready_session.svc.clone();
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
                        title: "会话 / 版本列表",
                        onclick: move |_| sessions_open.set(true),
                        History { size: "16" }
                    }
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

            // 会话/版本抽屉外壳：常驻受控，负责滑出动画、遮罩与关闭按钮。
            // 内容体仅在开启时挂载；关闭由 close/遮罩/esc 写回 sessions_open。
            Sheet {
                open: sessions_open(),
                on_open_change: move |v: bool| sessions_open.set(v),
                SheetHeader {
                    SheetTitle { "会话 / 版本" }
                    SheetContentClose {}
                }
                if sessions_open() {
                    SessionListSheet {
                        plan_id: props.plan_id.clone(),
                        on_select: move |session_id| {
                            // 挂载方决策：会话切换与 ChatService 重建在「会话监听」环节实现，
                            // 此处先记录，抽屉保持打开以便查看。
                            tracing::info!("会话列表选中: {session_id}");
                        },
                    }
                }
            }
        }
    }
}
