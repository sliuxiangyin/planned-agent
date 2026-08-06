//! ChatUIActionsView 组件：根据 UIActionType 分发渲染按钮/输入框。
//!
//! - Confirm / Select → 复用 Button 组件（Select 加虚线边框区分）
//! - Input → Input 组件 + 确认按钮，捕获用户自由文本后回调
//! - MultiSelect → 复选框组 + 配套 Confirm 按钮，读取勾选状态
//!
//! 所有用户操作统一通过 `on_action: EventHandler<(UIAction, String)>` 回调，
//! 由调用方决定如何写入 tool result 与继续对话。

use std::collections::HashMap;

use dioxus::prelude::*;
use planned_agent_core::types::{UIAction, UIActionType};

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::input::Input;

#[css_module("/src/pages/plan/components/chat_ui_actions_view/style.css")]
struct Styles;

/// Agent 请求用户交互时渲染的 UI 组件。
///
/// # Props
/// - `message` — 引导文本（如 "需要生成执行计划吗？"）
/// - `actions` — 用户可选的动作列表
/// - `on_action` — 用户操作回调，传入 (UIAction, 用户选择值)
///
/// # 安全网（防御 LLM 输出违规）
/// - Input + Select 混搭 → 只渲染 Input，丢弃 Select（两个不同问题不能混在一次交互）
/// - Input + Confirm 混搭 → 正常渲染（同一问题不同回答方式，如"输入路径 / 当前目录"）
/// - MultiSelect + Confirm → Confirm 按钮读取复选框勾选状态作为 choice 值
#[component]
pub fn ChatUIActionsView(
    message: String,
    actions: Vec<UIAction>,
    on_action: EventHandler<(UIAction, String)>,
) -> Element {
    let mut input_text = use_signal(String::new);

    // ── MultiSelect 复选框状态 ──
    let multi_select_actions: Vec<&UIAction> = actions
        .iter()
        .filter(|a| matches!(a.action_type, UIActionType::MultiSelect))
        .collect();
    let has_multi_select = !multi_select_actions.is_empty();
    let mut checkbox_state = use_signal(HashMap::<String, bool>::new);
    // 初始化默认勾选（仅首次）
    for ms in &multi_select_actions {
        for opt in &ms.options {
            checkbox_state.write().entry(opt.id.clone()).or_insert(opt.default);
        }
    }

    // ── 安全网：Input + Select 混搭 → 只保留 Input ──
    let input_actions: Vec<UIAction> = actions
        .iter()
        .filter(|a| matches!(a.action_type, UIActionType::Input))
        .cloned()
        .collect();
    let has_input = !input_actions.is_empty();
    let has_select = actions.iter().any(|a| matches!(a.action_type, UIActionType::Select));
    let show_button_group = !(has_input && has_select);

    if has_input && has_select {
        let button_count = actions.iter().filter(|a| !matches!(a.action_type, UIActionType::Input)).count();
        tracing::warn!(
            "ChatUIActionsView: LLM 返回 Input+Select 混搭（不同问题），丢弃 {} 个按钮 action，只渲染 Input",
            button_count
        );
    }

    // 按钮组数据（不含 Input / MultiSelect —— MultiSelect 单独渲染）
    let button_actions: Vec<UIAction> = actions
        .iter()
        .filter(|a| {
            !matches!(a.action_type, UIActionType::Input)
                && !matches!(a.action_type, UIActionType::MultiSelect)
        })
        .cloned()
        .collect();

    rsx! {
        div { class: Styles::chat_ui_actions,
            p { class: Styles::chat_ui_actions_message, "{message}" }

            // ── MultiSelect 复选框组 ──
            for ms in &multi_select_actions {
                for opt in &ms.options {
                    {
                        let opt_id = opt.id.clone();
                        let checked = checkbox_state.read().get(&opt_id).copied().unwrap_or(false);
                        rsx! {
                            label { class: Styles::checkbox_label,
                                input {
                                    r#type: "checkbox",
                                    checked: "{checked}",
                                    onchange: move |_| {
                                        let current = checkbox_state.read().get(&opt_id).copied().unwrap_or(false);
                                        checkbox_state.write().insert(opt_id.clone(), !current);
                                    },
                                }
                                "{opt.label}"
                            }
                        }
                    }
                }
            }

            // ── Input 类型：独占一行，先渲染 ──
            for action in &input_actions {
                {
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
                                size: ButtonSize::Sm,
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

            // ── Confirm / Select 按钮：flex-wrap 同行排列 ──
            if show_button_group {
                div { class: Styles::action_buttons,
                    for action in &button_actions {
                        {
                            match action.action_type {
                                UIActionType::Confirm => {
                                    let action = action.clone();
                                    let label = action.label.clone();
                                    let display_label = label.clone();
                                    let desc = action.description.clone().unwrap_or_default();
                                    // 若伴随 MultiSelect，choice 取复选框勾选 ID 集合
                                    let checkbox_state = checkbox_state.clone();
                                    rsx! {
                                        Button {
                                            variant: ButtonVariant::Secondary,
                                            size: ButtonSize::Sm,
                                            title: if desc.is_empty() { None } else { Some(desc) },
                                            onclick: move |_| {
                                                let choice = if has_multi_select {
                                                    let state = checkbox_state.read();
                                                    let ids: Vec<String> = state.iter()
                                                        .filter(|(_, &v)| v)
                                                        .map(|(k, _)| k.clone())
                                                        .collect();
                                                    if ids.is_empty() { "none".to_string() } else { ids.join(",") }
                                                } else {
                                                    label.clone()
                                                };
                                                on_action.call((action.clone(), choice));
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
                                                size: ButtonSize::Sm,
                                                title: if desc.is_empty() { None } else { Some(desc) },
                                                onclick: move |_| {
                                                    on_action.call((action.clone(), label.clone()));
                                                },
                                                "{display_label}"
                                            }
                                        }
                                    }
                                }
                                _ => rsx! {}
                            }
                        }
                    }
                }
            }
        }
    }
}
