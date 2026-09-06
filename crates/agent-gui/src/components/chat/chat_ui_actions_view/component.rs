//! ChatUIActionsView 组件：`request_user_action`「问题卡」渲染 —— 逐题向导式。
//!
//! 一次 `request_user_action` 可携带 ≤4 个并列独立问题；本组件**每次只展示一题**
//! 卡片，用户确认后切换到下一题，而不是把全部问题并排铺开。
//!
//! - 卡头：引导 `message`（可为空）+ 进度 `步骤 x/N`。
//! - 当前题：小标题 = `header`；`multi == false` → 单选 chip（**点选即选中并自动进入
//!   下一题**，末题除外）；`multi == true` → 复选框组（需手动点下一步）。
//! - `allow_input == true` 时显示「自定义回答」**按钮**（宽 100%，平时不占地方）；点击
//!   后原地展开输入框（reasonix 风格）。单选进入自定义会自动取消预设选中（自定义优先、
//!   单选保持单值）；填入后按钮态展示内容摘要并可一键清除。`allow_input == false`
//!   （该问 options 已穷尽）则不渲染该按钮。
//! - 底部导航：`取消`（跳过整批，回传空串）｜`上一步`（非首题）｜`下一步/提交`。
//! - 空题放行：某题未作答可直接「下一步」，回传时省略该行。
//! - 仅**最后一题的「提交」**触发 `on_submit`，把整批作答按 `header => answer` 多行
//!   一次回传；中途 `取消` 回传空串。协议仍为"一次调用 = 一次交互闭环"。
//!
//! 作答状态以 `question key`（header + 题序）存入顶层 signal，逐步索引存于
//! `current` signal；每次变更读 → 克隆 → 写回，避免长借用守卫。

use std::collections::{HashMap, HashSet};

use dioxus::prelude::*;
use planned_agent::UIQuestion;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::input::Input;

#[css_module("/src/components/chat/chat_ui_actions_view/style.css")]
struct Styles;

/// 单个问题的作答状态。
#[derive(Clone, Default)]
struct QState {
    /// 单选：当前勾选 option 下标（None = 未选）
    selected: Option<usize>,
    /// 多选：已勾选 option 下标集合
    checked: HashSet<usize>,
    /// allow_input 自由文本输入
    input: String,
    /// 「自定义回答」输入框是否展开中
    custom_open: bool,
}

/// 稳定唯一的问题键：header（同批唯一）加题序，防御 header 为空/重复。
fn qkey(header: &str, qi: usize) -> String {
    format!("{}#{}", header, qi)
}

/// 取一个 option 的机器值（无 value 回退到 label）。
fn opt_value(label: &str, value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| label.to_string())
}

/// 把每题答案打包成多行 `"{header} => {answer}"`。
///
/// 多选：勾选项 value（无 value 回 label）以 ", " 连接，`allow_input` 输入追加合并；
/// 单选：保持单值——`allow_input` 有输入时**以输入覆盖预设**（自定义优先），否则取所选
/// 预设的 value。空题（未作答且无输入）省略该行。
fn build_choice(questions: &[UIQuestion], map: &HashMap<String, QState>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (qi, q) in questions.iter().enumerate() {
        let Some(st) = map.get(&qkey(&q.header, qi)) else {
            continue;
        };
        let mut parts: Vec<String> = Vec::new();
        if q.multi {
            for (oi, opt) in q.options.iter().enumerate() {
                if st.checked.contains(&oi) {
                    parts.push(opt_value(&opt.label, &opt.value));
                }
            }
            let input = st.input.trim();
            if q.allow_input && !input.is_empty() {
                parts.push(input.to_string());
            }
        } else {
            // 单选：自定义优先——allow_input 输入非空即以输入覆盖预设（保持单值语义）；
            // 否则取所选预设；未选预设且无输入视为空题、回传省略该行。
            let input = st.input.trim();
            if q.allow_input && !input.is_empty() {
                parts.push(input.to_string());
            } else if let Some(oi) = st.selected {
                if let Some(opt) = q.options.get(oi) {
                    parts.push(opt_value(&opt.label, &opt.value));
                }
            }
        }
        if !parts.is_empty() {
            lines.push(format!("{} => {}", q.header, parts.join(", ")));
        }
    }
    lines.join("\n")
}

/// Agent 请求用户交互时渲染的 UI 组件（逐题向导式问题卡）。
///
/// # Props
/// - `message` — 顶部引导文本（可为空）
/// - `questions` — 并列问题数组，每问含 options/multi/allow_input
/// - `on_submit` — 末题「提交」或「取消」时回调，传入打包选择字符串（取消传空串）
#[component]
pub fn ChatUIActionsView(
    message: String,
    questions: Vec<UIQuestion>,
    on_submit: EventHandler<String>,
) -> Element {
    // 各题作答状态（key → QState）
    let mut states = use_signal(|| HashMap::<String, QState>::new());
    // 当前展示的题序
    let mut current = use_signal(|| 0usize);

    let n = questions.len();
    if n == 0 {
        // 兜底：正常情况下服务端已拦截空 questions（视为参数损坏的 request_user_action），
        // 不会发送到前端。若仍有残余，渲染一条可见说明而非空白卡区，避免看起来"卡住"。
        return rsx! {
            div { class: Styles::chat_ui_actions,
                div { class: Styles::wizard_header,
                    span { class: Styles::chat_ui_actions_message, "{message}" }
                }
                div { class: Styles::question_card,
                    div { class: Styles::question_text, "请求未携带可交互的选项，无法继续。" }
                }
            }
        };
    }

    let qi = current().min(n - 1);
    let q = &questions[qi];
    let key = qkey(&q.header, qi);
    let is_last = qi == n - 1;

    // 顶部引导 + 进度
    let steps_text = format!("步骤 {}/{}", qi + 1, n);

    rsx! {
        div { class: Styles::chat_ui_actions,
            div { class: Styles::wizard_header,
                span { class: Styles::chat_ui_actions_message, "{message}" }
                span { class: Styles::wizard_progress, "{steps_text}" }
            }

            // ── 当前问题卡片 ──
            div { class: Styles::question_card,
                if !q.header.is_empty() {
                    div { class: Styles::question_header, "{q.header}" }
                }
                if !q.question.is_empty() {
                    div { class: Styles::question_text, "{q.question}" }
                }

                // ── 单选（multi == false）：互斥 chip，点选即选中并自动进入下一题 ──
                if !q.multi {
                    div { class: Styles::action_buttons,
                        for (oi, opt) in q.options.iter().enumerate() {
                            {
                                let key = key.clone();
                                let is_sel = { states.read().get(&key).cloned().unwrap_or_default().selected == Some(oi) };
                                let label = opt.label.clone();
                                let title = opt.description.clone().unwrap_or_default();
                                let not_last = !is_last;
                                rsx! {
                                    Button {
                                        variant: if is_sel { ButtonVariant::Primary } else { ButtonVariant::Secondary },
                                        size: ButtonSize::Sm,
                                        title: if title.is_empty() { None } else { Some(title) },
                                        onclick: move |_| {
                                            let mut cur = states.read().get(&key).cloned().unwrap_or_default();
                                            let selecting = cur.selected != Some(oi);
                                            cur.selected = if cur.selected == Some(oi) { None } else { Some(oi) };
                                            states.write().insert(key.clone(), cur);
                                            // 单选刚点选某选项：非末题自动进入下一题；末题等待底部「提交」
                                            if selecting && not_last {
                                                current += 1;
                                            }
                                        },
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }
                }

                // ── 多选（multi == true）：复选框组，需手动「下一步」 ──
                if q.multi {
                    div { class: Styles::multi_select_group,
                        for (oi, opt) in q.options.iter().enumerate() {
                            {
                                let key = key.clone();
                                let checked = states.read().get(&key).cloned().unwrap_or_default().checked.contains(&oi);
                                let label = opt.label.clone();
                                let title = opt.description.clone().unwrap_or_default();
                                rsx! {
                                    label {
                                        class: Styles::checkbox_label,
                                        title: if title.is_empty() { None } else { Some(title) },
                                        input {
                                            r#type: "checkbox",
                                            checked: checked,
                                            onchange: move |_| {
                                                let mut cur = states.read().get(&key).cloned().unwrap_or_default();
                                                if cur.checked.contains(&oi) {
                                                    cur.checked.remove(&oi);
                                                } else {
                                                    cur.checked.insert(oi);
                                                }
                                                states.write().insert(key.clone(), cur);
                                            },
                                        }
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }
                }

                // ── allow_input：该问的「自定义回答」按钮（点击后原地展开输入框） ──
                if q.allow_input {
                    {
                        let cur = states.read().get(&key).cloned().unwrap_or_default();
                        let open = cur.custom_open;
                        let input_text = cur.input;
                        let has_input = !input_text.trim().is_empty();
                        // 已填摘要：非空时按钮文案显示截断内容，完整文本放 title
                        let trimmed = input_text.trim();
                        let summary = if trimmed.is_empty() {
                            String::new()
                        } else {
                            let s: String = trimmed.chars().take(20).collect();
                            if trimmed.chars().count() > 20 { format!("{s}…") } else { s }
                        };
                        let label = if has_input { summary } else { "自定义回答".to_string() };
                        let is_single = !q.multi;
                        rsx! {
                            if open {
                                div { class: Styles::custom_edit_row,
                                    {
                                        let key = key.clone();
                                        let input_val = states.read().get(&key).cloned().unwrap_or_default().input;
                                        let ph = if q.question.is_empty() { q.header.clone() } else { q.question.clone() };
                                        rsx! {
                                            Input {
                                                placeholder: "{ph}",
                                                value: "{input_val}",
                                                oninput: move |e: FormEvent| {
                                                    let mut cur = states.read().get(&key).cloned().unwrap_or_default();
                                                    cur.input = e.value();
                                                    states.write().insert(key.clone(), cur);
                                                },
                                            }
                                        }
                                    }
                                    div { class: Styles::custom_edit_actions,
                                        {
                                            let key = key.clone();
                                            rsx! {
                                                Button {
                                                    variant: ButtonVariant::Ghost,
                                                    size: ButtonSize::Sm,
                                                    onclick: move |_| {
                                                        let mut cur = states.read().get(&key).cloned().unwrap_or_default();
                                                        cur.input.clear();
                                                        cur.custom_open = false;
                                                        states.write().insert(key.clone(), cur);
                                                    },
                                                    "清除"
                                                }
                                            }
                                        }
                                        {
                                            let key = key.clone();
                                            rsx! {
                                                Button {
                                                    variant: ButtonVariant::Secondary,
                                                    size: ButtonSize::Sm,
                                                    onclick: move |_| {
                                                        let mut cur = states.read().get(&key).cloned().unwrap_or_default();
                                                        cur.custom_open = false;
                                                        states.write().insert(key.clone(), cur);
                                                    },
                                                    "完成"
                                                }
                                            }
                                        }
                                    }
                                }
                            } else {
                                div { class: Styles::custom_trigger_row,
                                    {
                                        let key = key.clone();
                                        let label = label.clone();
                                        let tip = if has_input {
                                            "点此修改自定义回答".to_string()
                                        } else {
                                            "输入自定义回答".to_string()
                                        };
                                        rsx! {
                                            Button {
                                                variant: if has_input { ButtonVariant::Secondary } else { ButtonVariant::Ghost },
                                                size: ButtonSize::Sm,
                                                class: Styles::custom_trigger_btn,
                                                title: tip,
                                                onclick: move |_| {
                                                    let mut cur = states.read().get(&key).cloned().unwrap_or_default();
                                                    cur.custom_open = true;
                                                    // 单选进入自定义：取消预设选中（自定义优先，保持单选单值）
                                                    if is_single {
                                                        cur.selected = None;
                                                    }
                                                    states.write().insert(key.clone(), cur);
                                                },
                                                "{label}"
                                            }
                                        }
                                    }
                                    if has_input {
                                        {
                                            let key = key.clone();
                                            rsx! {
                                                Button {
                                                    variant: ButtonVariant::Ghost,
                                                    size: ButtonSize::IconSm,
                                                    class: Styles::custom_clear_btn,
                                                    title: "清除自定义回答",
                                                    onclick: move |_| {
                                                        let mut cur = states.read().get(&key).cloned().unwrap_or_default();
                                                        cur.input.clear();
                                                        cur.custom_open = false;
                                                        states.write().insert(key.clone(), cur);
                                                    },
                                                    "✕"
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

            // ── 底部导航：左「取消」，右侧「上一步 / 下一步(末题为提交)」 ──
            div { class: Styles::wizard_nav,
                {
                    let on_submit = on_submit.clone();
                    rsx! {
                        Button {
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::Sm,
                            title: "取消本轮交互，整体跳过",
                            onclick: move |_| on_submit.call(String::new()),
                            "取消"
                        }
                    }
                }
                div { class: Styles::wizard_nav_actions,
                    {
                        if qi > 0 {
                            let prev = qi - 1;
                            rsx! {
                                Button {
                                    variant: ButtonVariant::Secondary,
                                    size: ButtonSize::Sm,
                                    onclick: move |_| current.set(prev),
                                    "上一步"
                                }
                            }
                        } else {
                            rsx! {}
                        }
                    }
                    {
                        let questions = questions.clone();
                        let label = if is_last { "提交" } else { "下一步" };
                        rsx! {
                            Button {
                                variant: ButtonVariant::Primary,
                                size: ButtonSize::Sm,
                                onclick: move |_| {
                                    if is_last {
                                        // 末题：打包整批回传
                                        let map = states.read();
                                        let choice = build_choice(&questions, &map);
                                        on_submit.call(choice);
                                    } else {
                                        // 非末题：空题也放行
                                        current += 1;
                                    }
                                },
                                "{label}"
                            }
                        }
                    }
                }
            }
        }
    }
}
