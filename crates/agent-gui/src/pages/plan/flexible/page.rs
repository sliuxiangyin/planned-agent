//! 灵活模式主组件：基于 chat 的聊天面板（含子 agent 支持）。
//!
//! 这是纯视图层：所有 signal / 初始化 / 事件逻辑都收在 `use_flexible_controller`
//! （见 `controller.rs`）。本组件只做「读控制器状态 → 渲染对应 UI」。
//!
//! 初始化未完成时渲染占位；完成后把 controller 的状态与方法喂给通用 `ChatPanel`。

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

/// 自定义 hook：把 `SessionManager` 的「当前会话」同步到 `session_id` 信号。
///
/// `SessionManager` 的 tokio watch 不驱动重渲染，故订阅其 dioxus 桥 `current()`；
/// `set()` 会同时写 watch 与该信号，二者保持一致。
///
/// 必须在组件渲染期无条件、顺序稳定地调用（遵守 rules of hooks）。
fn use_listen_session_manager(mut session_id: Signal<String>) {
    let session_mgr = use_context::<Arc<SessionManager>>();
    use_effect(move || {
        let current = session_mgr.current();
        let value = current.read();

        if let Some(id) = &*value {
            // Signal::set 不做相等判断，同值写入也会通知订阅者；
            // 故先比对现值，避免用相同 id 二次触发监听 session_id 的 effect。
            // 注意用 peek() 读：只取值、不建立对 session_id 的订阅，
            // 否则本 effect 会反过来依赖 session_id（破坏 effect 间隔离）。
            if session_id.peek().as_str() != id.as_str() {
                session_id.set(id.clone());
            }
        }
    });
}

#[component]
pub fn FlexiblePage(props: FlexiblePageProps) -> Element {
    // 页面持有的 session_id 响应式信号：初值来自 props，随后跟随 SessionManager
    // 的「当前会话」变化而更新。
    let initial_session_id = props.session_id.clone();
    let session_id = use_signal(move || initial_session_id.clone());

    // 订阅 SessionManager 的「当前会话」，同步到 session_id（详见 hook 定义）。
    use_listen_session_manager(session_id);
    let ctl = use_flexible_controller(props.plan_id.clone(), session_id);

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

    let bridge = ready_session.bridge.clone();
    let busy = ctl.is_busy();
    let current_template = ctl.template();
    let template_label = crate::components::chat::chat_panel::template_label(&current_template);

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
}
