//! ChatUIActionsView 组件：根据 UIActionType 分发渲染按钮/输入框。
//!
//! - Confirm / Select → 复用 Button 组件（Select 加虚线边框区分）
//! - Input → Input 组件 + 确认按钮，捕获用户自由文本后回调
//!
//! 所有用户操作统一通过 `on_action: EventHandler<(UIAction, String)>` 回调，
//! 由调用方决定如何写入 tool result 与继续对话。

use dioxus::prelude::*;
use planned_agent_core::types::{UIAction, UIActionType};

use crate::components::button::{Button, ButtonVariant};
use crate::components::input::Input;

#[css_module("/src/pages/plan/components/chat_ui_actions_view/style.css")]
struct Styles;

/// Agent 请求用户交互时渲染的 UI 组件。
///
/// # Props
/// - `message` — 引导文本（如 "需要生成执行计划吗？"）
/// - `actions` — 用户可选的动作列表
/// - `on_action` — 用户操作回调，传入 (UIAction, 用户选择值)
#[component]
pub fn ChatUIActionsView(
    message: String,
    actions: Vec<UIAction>,
    on_action: EventHandler<(UIAction, String)>,
) -> Element {
    // Input 类型的本地输入状态（实践中 actions 数组至多含一个 Input action）
    let mut input_text = use_signal(String::new);

    rsx! {
        div { class: Styles::chat_ui_actions,
            p { class: Styles::chat_ui_actions_message, "{message}" }
            div { class: Styles::chat_ui_actions_buttons,
                for action in &actions {
                    {
                        match action.action_type {
                            UIActionType::Confirm => {
                                let action = action.clone();
                                let label = action.label.clone();
                                let display_label = label.clone();
                                let desc = action.description.clone().unwrap_or_default();
                                rsx! {
                                    Button {
                                        variant: ButtonVariant::Secondary,
                                        title: if desc.is_empty() { None } else { Some(desc) },
                                        onclick: move |_| {
                                            on_action.call((action.clone(), label.clone()));
                                        },
                                        "{display_label}"
                                    }
                                }
                            }
                            UIActionType::Select => {
                                let action = action.clone();
                                let label = action.label.clone();
                                let display_label = label.clone();
                                let desc = action.description.clone().unwrap_or_default();
                                rsx! {
                                    span { class: Styles::select_btn,
                                        Button {
                                            variant: ButtonVariant::Secondary,
                                            title: if desc.is_empty() { None } else { Some(desc) },
                                            onclick: move |_| {
                                                on_action.call((action.clone(), label.clone()));
                                            },
                                            "{display_label}"
                                        }
                                    }
                                }
                            }
                            UIActionType::Input => {
                                let action = action.clone();
                                let enter_action = action.clone();
                                let click_action = action;
                                let placeholder = enter_action.description.clone().unwrap_or_default();
                                rsx! {
                                    div { class: Styles::input_row,
                                        Input {
                                            placeholder: "{placeholder}",
                                            value: "{input_text}",
                                            oninput: move |e: FormEvent| input_text.set(e.value()),
                                            onkeydown: move |e: KeyboardEvent| {
                                                if e.data.key() == keyboard_types::Key::Enter {
                                                    let v = input_text.read().trim().to_string();
                                                    if !v.is_empty() {
                                                        on_action.call((enter_action.clone(), v));
                                                        input_text.set(String::new());
                                                    }
                                                }
                                            },
                                        }
                                        Button {
                                            variant: ButtonVariant::Secondary,
                                            onclick: move |_| {
                                                let v = input_text.read().trim().to_string();
                                                if !v.is_empty() {
                                                    on_action.call((click_action.clone(), v));
                                                    input_text.set(String::new());
                                                }
                                            },
                                            "确认"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
