//! ChatUIActionsView 组件：根据 UIActionType 分发渲染按钮/输入框。
//!
//! - Confirm / Select → 复用 Button 组件（Select 加虚线边框区分）
//!   Select 单选列表末尾自动附带「自定义输入」入口（D）：类比 reasonix 追问，
//!   预设选项（A/B/C 按钮）之外允许用户手动输入补充，输入文本作为 choice 回传
//! - Input → Input 组件 + 确认按钮，捕获用户自由文本后回调
//! - MultiSelect → 复选框组 + 配套 Confirm 按钮，读取勾选状态
//!
//! 所有用户操作统一通过 `on_action: EventHandler<(UIAction, String)>` 回调，
//! 由调用方决定如何写入 tool result 与继续对话。

use std::collections::HashMap;

use dioxus::prelude::*;
use planned_agent::{FALLBACK_CONFIRM_ID, FALLBACK_CONFIRM_LABEL, UIAction, UIActionType};

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::input::Input;

#[css_module("/src/components/chat/chat_ui_actions_view/style.css")]
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
/// - 纯 MultiSelect（无任何 confirm/select/input）→ 自动补「确定」按钮读取勾选状态，
///   防止只有复选框、无提交入口导致交互卡死（后端 service 层也会自动补齐，此处为二道防线）
#[component]
pub fn ChatUIActionsView(
    message: String,
    actions: Vec<UIAction>,
    on_action: EventHandler<(UIAction, String)>,
) -> Element {
    let mut input_text = use_signal(String::new);
    // Select 单选组「自定义输入」（D）的文本状态（独立于 Input 的 input_text）
    let mut select_custom_text = use_signal(String::new);

    // ── MultiSelect 复选框状态 ──
    let multi_select_actions: Vec<&UIAction> = actions
        .iter()
        .filter(|a| matches!(a.action_type, UIActionType::MultiSelect))
        .collect();
    let has_multi_select = !multi_select_actions.is_empty();
    let mut checkbox_state = use_signal(HashMap::<String, bool>::new);
    // option id → value 映射（用于构造 "id=value" 格式 choice）
    let mut option_value_map = use_signal(HashMap::<String, Option<String>>::new);
    // 初始化默认勾选 + value 映射（仅首次）
    for ms in &multi_select_actions {
        for opt in &ms.options {
            checkbox_state.write().entry(opt.id.clone()).or_insert(opt.default);
            option_value_map.write().entry(opt.id.clone()).or_insert(opt.value.clone());
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
    // Select 单选组：预设选项按钮之外的「自定义输入」入口（D）
    let select_actions: Vec<UIAction> = actions
        .iter()
        .filter(|a| matches!(a.action_type, UIActionType::Select))
        .cloned()
        .collect();
    // 自定义输入（D）的回传目标：组内第一个 action。一次 request_user_action 只针对一个
    // 决策点（prompt 约定），select 组即该单选问题本身；D 与点击按钮同属此决策点，归到
    // 组内第一个 action 的 id 语义安全（handle_user_action 仅关心 choice 文本与 id=="generate"）。
    let select_custom_target: Option<UIAction> = select_actions.first().cloned();
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

    // ── 多选勾选 → choice 字符串（"id=value" 逗号拼接；未勾选回 "none"）──
    let build_multi_choice = move || {
        let state = checkbox_state.read();
        let values = option_value_map.read();
        let ids: Vec<String> = state
            .iter()
            .filter(|(_, &v)| v)
            .filter_map(|(k, _)| {
                values
                    .get(k)
                    .and_then(|v| v.as_ref())
                    // 有 value → id=value；无 value → 仅回传 id（schema 允许 value 缺省，不丢弃勾选项）
                    .map(|val| format!("{}={}", k, val))
                    .or_else(|| Some(k.clone()))
            })
            .collect();
        if ids.is_empty() {
            "none".to_string()
        } else {
            ids.join(",")
        }
    };

    // ── 兜底：有 MultiSelect 但无 confirm 提交按钮（LLM 违规）→ 自动补「确定」按钮，防止交互卡死 ──
    let has_confirm_btn = actions
        .iter()
        .any(|a| matches!(a.action_type, UIActionType::Confirm));
    let need_fallback_confirm = has_multi_select && !has_confirm_btn;
    if need_fallback_confirm {
        tracing::warn!(
            "ChatUIActionsView: MultiSelect 无 confirm 按钮（LLM 违规），自动补「确定」按钮"
        );
    }
    let fallback_action = UIAction {
        id: FALLBACK_CONFIRM_ID.to_string(),
        action_type: UIActionType::Confirm,
        label: FALLBACK_CONFIRM_LABEL.to_string(),
        description: None,
        options: vec![],
    };

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

            // ── 兜底：MultiSelect 自动补的「确定」按钮（读取勾选状态）──
            if need_fallback_confirm {
                div { class: Styles::action_buttons,
                    {
                        let fallback_action = fallback_action.clone();
                        let build = build_multi_choice.clone();
                        rsx! {
                            Button {
                                variant: ButtonVariant::Secondary,
                                size: ButtonSize::Sm,
                                onclick: move |_| {
                                    on_action.call((fallback_action.clone(), build()));
                                },
                                "{FALLBACK_CONFIRM_LABEL}"
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
                                    let build = build_multi_choice.clone();
                                    let has_ms = has_multi_select;
                                    rsx! {
                                        Button {
                                            variant: ButtonVariant::Secondary,
                                            size: ButtonSize::Sm,
                                            title: if desc.is_empty() { None } else { Some(desc) },
                                            onclick: move |_| {
                                                let choice = if has_ms {
                                                    build()
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

            // ── Select 单选组「自定义输入」入口（D）──
            if show_button_group && has_select {
                for action in select_custom_target.iter() {
                    {
                        let enter_action = action.clone();
                        let click_action = action.clone();
                        rsx! {
                            div { class: Styles::select_custom_row,
                                Input {
                                    placeholder: "或输入其他选项…",
                                    value: "{select_custom_text}",
                                    oninput: move |e: FormEvent| select_custom_text.set(e.value()),
                                    onkeydown: move |e: KeyboardEvent| {
                                        if e.data.key() == keyboard_types::Key::Enter {
                                            let v = select_custom_text.read().trim().to_string();
                                            if !v.is_empty() {
                                                on_action.call((enter_action.clone(), v));
                                                select_custom_text.set(String::new());
                                            }
                                        }
                                    },
                                }
                                Button {
                                    variant: ButtonVariant::Secondary,
                                    size: ButtonSize::Sm,
                                    onclick: move |_| {
                                        let v = select_custom_text.read().trim().to_string();
                                        if !v.is_empty() {
                                            on_action.call((click_action.clone(), v));
                                            select_custom_text.set(String::new());
                                        }
                                    },
                                    "补充"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
