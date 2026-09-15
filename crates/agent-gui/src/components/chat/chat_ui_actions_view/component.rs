//! ChatUIActionsView 组件：`request_user_action`「问题卡」渲染 —— 逐题向导式。
//!
//! 一次 `request_user_action` 可携带 ≤4 个并列独立问题；本组件**每次只展示一题**
//! 卡片，用户确认后切换到下一题，而不是把全部问题并排铺开。
//!
//! ## 版式（参考 Qoder IDE 的 UI action 卡片）
//!
//! ```text
//! 请回答以下问题                                   1 / 4
//! ──────────────────────────────────────────────────────
//! 今天午餐想吃什么？   (单选)
//!  A   热辣过瘾，适合多人聚餐
//!  B   滋滋冒油，配啤酒绝了   [推荐]      ← 整行高亮 = 当前选中
//!  C   健康轻食，适合减脂
//!  D   简单快捷，中式经典
//!  E   [ 输入自定义答案        ]          ← allow_input 时的最后一行（常驻输入框）
//! ──────────────────────────────────────────────────────
//! [✏️ 推荐选项]                     [取消]  [继续]
//! ```
//!
//! - 卡头：引导 `message`（可为空）+ 右上角页码 `当前/N`。
//! - 当前题：`header`（小标签）+ `question`（正文）+ `(单选)/(多选)` 标注。
//! - **选项列表**：单选与多选**共用同一套行样式**（行首 `A/B/C/D` 字母序号 +
//!   `label` 主文案 + `description` 灰色副文案 + 可选「推荐」角标），整行可点。
//!   两者的区别只有选择语义：`multi == false` 互斥、`multi == true` 可多行同时选中。
//! - **推进**：单选**点选即自动进入下一题**（末题除外，等内容后点「继续」提交）；
//!   多选需手动点「继续」。
//! - **继续**：当前题未作答（未选中任何行且自定义输入为空）时**置灰不可点**——
//!   即不再支持空题跳过。
//! - `allow_input == true` 时，选项列表末尾多一行「输入自定义答案」：该行的内容区是
//!   **常驻的行内输入框**（`placeholder` 即提示语），点它就能直接打字，没有展开/折叠
//!   两态，也不需要「清除 / 完成」按钮。输入非空时整行高亮（与选中态一致）；单选下
//!   **输入了自定义内容**才会取消预设选中（单纯点一下输入框不算作答，预设仍在）。
//! - **推荐项**：`options[].recommended` 标出的行带「推荐」角标；底部左下角
//!   「✏️ 推荐选项」按钮可一键采纳（单选 = 选中并推进、多选 = 勾上）；本题没有
//!   推荐项时该按钮置灰。
//! - **键盘**：卡片聚焦时按 `A`–`F` 直接选中对应行（带 Ctrl/Alt/Cmd 的组合键不拦截，
//!   照常交给浏览器）；按到「输入自定义答案」行（末行）时把光标移进那个输入框。
//!
//! 作答状态以 `question key`（header + 题序）存入顶层 signal，逐步索引存于
//! `current` signal；每次变更读 → 克隆 → 写回，避免长借用守卫。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use dioxus::prelude::*;
use planned_agent::UIQuestion;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::chat::chat_flow::ActionReply;

#[css_module("/src/components/chat/chat_ui_actions_view/style.css")]
struct Styles;

/// 卡片实例自增序号：同页可能同时存在多张卡片（历史 + 当前），
/// 用于生成唯一的输入框 DOM id（键盘/点击聚焦时要精确命中本卡片的那个）。
static CARD_SEQ: AtomicUsize = AtomicUsize::new(0);

/// 单个问题的作答状态。
#[derive(Clone, Default)]
struct QState {
    /// 已选中的选项下标：单选恒为 0 或 1 个，多选可多个（统一用集合，渲染只有一套）
    picked: HashSet<usize>,
    /// allow_input 的自由文本输入（「输入自定义答案」行的内容）
    input: String,
}

impl QState {
    /// 该题是否已作答（选了行，或自定义输入非空）——决定「继续」是否可点。
    fn answered(&self) -> bool {
        !self.picked.is_empty() || !self.input.trim().is_empty()
    }
}

/// 稳定唯一的问题键：header（同批唯一）加题序，防御 header 为空/重复。
fn qkey(header: &str, qi: usize) -> String {
    format!("{}#{}", header, qi)
}

/// 取一个 option 的机器值（无 value 回退到 label）。
fn opt_value(label: &str, value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| label.to_string())
}

/// 行首字母序号（A、B、C…；单题最多 5 个选项 + 1 个自定义答案行，不会超过 Z）。
fn option_letter(idx: usize) -> char {
    (b'A' + idx.min(25) as u8) as char
}

/// 把按键字母映射到行下标（`A`/`a` → 0，`B`/`b` → 1，…）。
fn letter_index(s: &str) -> Option<usize> {
    let c = s.chars().next()?;
    if !c.is_ascii_alphabetic() {
        return None;
    }
    Some((c.to_ascii_uppercase() as u8 - b'A') as usize)
}

/// 该题的推荐项下标（模型标注 `recommended`；万一标了多个取第一个）。
fn recommended_index(q: &UIQuestion) -> Option<usize> {
    q.options.iter().position(|o| o.recommended)
}

/// 「输入自定义答案」行输入框的 DOM id（卡片实例 + 题序，供键盘/点击聚焦该输入框）。
fn custom_input_id(card_id: usize, qi: usize) -> String {
    format!("rua-custom-input-{card_id}-{qi}")
}

/// 选中某个**预设选项行**后的状态更新。
///
/// - 多选 = 切换该行勾选；单选 = 改选（再点已选中那行则取消）。
/// - 单选下改选会清掉自定义输入——单选保持单值，预设与自定义互斥；反向由输入框的
///   `oninput` 负责（真的填了非空内容才清空预设选中，仅点一下输入框不算）。
/// - 单选**新选中**某项且非末题 → 自动进入下一题；末题等内容后点「继续」提交。
/// - `idx` 落在「输入自定义答案」行（`idx >= options_len`）时无操作：那一行由它自己的
///   输入框处理输入与焦点。
fn apply_pick(
    mut states: Signal<HashMap<String, QState>>,
    mut current: Signal<usize>,
    key: &str,
    idx: usize,
    multi: bool,
    is_last: bool,
    options_len: usize,
) {
    if idx >= options_len {
        return;
    }

    let mut cur = states.read().get(key).cloned().unwrap_or_default();
    let was_picked = cur.picked.contains(&idx);
    if multi {
        if was_picked {
            cur.picked.remove(&idx);
        } else {
            cur.picked.insert(idx);
        }
    } else {
        cur.picked.clear();
        if !was_picked {
            cur.picked.insert(idx);
            cur.input.clear(); // 单选：改用预设，自定义让位
        }
    }
    states.write().insert(key.to_string(), cur);

    if !multi && !was_picked && !is_last {
        current.set(current() + 1);
    }
}

/// 把每题答案打包成多行 `"{header} => {answer}"`。
///
/// 多选：勾选项 value（无 value 回 label）以 ", " 连接，`allow_input` 输入追加合并；
/// 单选：保持单值——`allow_input` 有输入时**以输入覆盖预设**（自定义优先），否则取所选
/// 预设的 value。空题（未作答且无输入）省略该行（新交互下「继续」已置灰，正常不会出现）。
fn build_choice(questions: &[UIQuestion], map: &HashMap<String, QState>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (qi, q) in questions.iter().enumerate() {
        let Some(st) = map.get(&qkey(&q.header, qi)) else {
            continue;
        };
        let mut parts: Vec<String> = Vec::new();
        if q.multi {
            for (oi, opt) in q.options.iter().enumerate() {
                if st.picked.contains(&oi) {
                    parts.push(opt_value(&opt.label, &opt.value));
                }
            }
            let input = st.input.trim();
            if q.allow_input && !input.is_empty() {
                parts.push(input.to_string());
            }
        } else {
            // 单选：自定义优先——allow_input 输入非空即以输入覆盖预设（保持单值语义）；
            // 否则取所选预设（picked 恒 ≤1 项，取任一项即该项）；未选预设且无输入视为空题、
            // 回传省略该行。
            let input = st.input.trim();
            if q.allow_input && !input.is_empty() {
                parts.push(input.to_string());
            } else if let Some(oi) = st.picked.iter().next().copied() {
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

/// Agent 请求用户交互时渲染的 UI 组件（逐题向导式问题卡，Qoder 风格统一选项列表）。
///
/// # Props
/// - `message` — 顶部引导文本（可为空）
/// - `questions` — 并列问题数组，每问含 options/multi/allow_input
/// - `on_submit` — 末题「继续」回传 `ActionReply::Submit(打包选择字符串)`；「取消」回传
///   `ActionReply::Cancel`。二者语义分离，不再是同一个空串。
#[component]
pub fn ChatUIActionsView(
    message: String,
    questions: Vec<UIQuestion>,
    on_submit: EventHandler<ActionReply>,
) -> Element {
    // 各题作答状态（key → QState）
    let mut states = use_signal(|| HashMap::<String, QState>::new());
    // 当前展示的题序
    let mut current = use_signal(|| 0usize);
    // 本卡片实例序号（渲染期生成，生命周期内不变）——用于输入框 DOM id
    let card_id = use_hook(|| CARD_SEQ.fetch_add(1, Ordering::Relaxed));

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

    let options_len = q.options.len();
    let multi = q.multi;
    let allow_input = q.allow_input;
    // 列表总行数 = 预设选项 + （可选）自定义答案行
    let row_count = options_len + usize::from(allow_input);
    let reco_idx = recommended_index(q);
    let page_text = format!("{} / {}", qi + 1, n);
    let kind_text = if multi { "多选" } else { "单选" };
    let input_dom_id = custom_input_id(card_id, qi);

    let state_now = states.read().get(&key).cloned().unwrap_or_default();
    let answered = state_now.answered();
    // 自定义答案行已填内容 → 整行按选中态高亮（与预设选项行一致）
    let custom_active = allow_input && !state_now.input.trim().is_empty();

    rsx! {
        div {
            class: Styles::chat_ui_actions,
            // 让卡片可聚焦：聚焦后 A–F 快捷键生效（挂载时自动聚焦一次）
            tabindex: "0",
            onmounted: move |e: MountedEvent| {
                spawn(async move {
                    let _ = e.set_focus(true).await;
                });
            },
            onkeydown: {
                let key_kb = key.clone();
                let dom_id = input_dom_id.clone();
                move |e: KeyboardEvent| {
                    let pressed = e.data().key();
                    let keyboard_types::Key::Character(ch) = pressed else {
                        return;
                    };
                    // 带 Ctrl/Alt/Cmd 的组合键（复制、粘贴、全选…）交给浏览器，不当选行处理
                    let mods = e.modifiers();
                    if mods.contains(keyboard_types::Modifiers::CONTROL)
                        || mods.contains(keyboard_types::Modifiers::ALT)
                        || mods.contains(keyboard_types::Modifiers::META)
                    {
                        return;
                    }
                    let s: &str = &ch;
                    let Some(idx) = letter_index(s) else {
                        return;
                    };
                    if idx >= row_count {
                        return;
                    }
                    e.prevent_default();
                    if idx == options_len {
                        // 「输入自定义答案」行：把光标移进那一行的输入框
                        let _ = document::eval(&format!(
                            "document.getElementById('{dom_id}')?.focus();"
                        ));
                        return;
                    }
                    apply_pick(states, current, &key_kb, idx, multi, is_last, options_len);
                }
            },

            // ── 卡头：引导文本 + 右上角页码 ──
            div { class: Styles::wizard_header,
                span { class: Styles::chat_ui_actions_message, "{message}" }
                span { class: Styles::wizard_progress, "{page_text}" }
            }

            // ── 当前问题卡片 ──
            div { class: Styles::question_card,
                div { class: Styles::question_title_row,
                    if !q.header.is_empty() {
                        span { class: Styles::question_header, "{q.header}" }
                    }
                    if !q.question.is_empty() {
                        span { class: Styles::question_text, "{q.question}" }
                    }
                    span { class: Styles::question_kind, "{kind_text}" }
                }

                // ── 选项列表：单选/多选共用同一套行样式 ──
                div { class: Styles::options_list,
                    for (oi, opt) in q.options.iter().enumerate() {
                        {
                            let row_key = key.clone();
                            let picked = state_now.picked.contains(&oi);
                            let picked_attr = if picked { "true" } else { "false" };
                            let reco_attr = if opt.recommended { "true" } else { "false" };
                            let letter = option_letter(oi).to_string();
                            let label = opt.label.clone();
                            let desc = opt.description.clone().unwrap_or_default();
                            let is_reco = opt.recommended;
                            rsx! {
                                div {
                                    key: "opt-{oi}",
                                    class: Styles::option_row,
                                    "data-picked": picked_attr,
                                    "data-recommended": reco_attr,
                                    onclick: move |_| {
                                        apply_pick(states, current, &row_key, oi, multi, is_last, options_len);
                                    },
                                    span { class: Styles::option_letter, "{letter}" }
                                    div { class: Styles::option_body,
                                        span { class: Styles::option_label, "{label}" }
                                        if !desc.is_empty() {
                                            span { class: Styles::option_desc, "{desc}" }
                                        }
                                    }
                                    if is_reco {
                                        span { class: Styles::recommended_badge, "推荐" }
                                    }
                                }
                            }
                        }
                    }

                    // ── allow_input：「输入自定义答案」行（列表末行，行内常驻输入框） ──
                    if allow_input {
                        {
                            let input_key = key.clone();
                            let click_id = input_dom_id.clone();
                            let input_val = state_now.input.clone();
                            let letter = option_letter(options_len).to_string();
                            let picked_attr = if custom_active { "true" } else { "false" };
                            rsx! {
                                div {
                                    key: "custom-{qi}",
                                    class: Styles::option_row,
                                    "data-picked": picked_attr,
                                    "data-custom": "true",
                                    // 点这一行的任何位置（字母序号/内边距）都把光标送进输入框
                                    onclick: move |_| {
                                        let _ = document::eval(&format!(
                                            "document.getElementById('{click_id}')?.focus();"
                                        ));
                                    },
                                    span { class: Styles::option_letter, "{letter}" }
                                    input {
                                        id: "{input_dom_id}",
                                        class: Styles::custom_input,
                                        r#type: "text",
                                        placeholder: "输入自定义答案",
                                        value: "{input_val}",
                                        // 输入框内的按键不冒泡到卡片，避免打字时触发 A–F 选行
                                        onkeydown: move |e: KeyboardEvent| e.stop_propagation(),
                                        oninput: move |e: FormEvent| {
                                            let mut cur = states.read().get(&input_key).cloned().unwrap_or_default();
                                            cur.input = e.value();
                                            // 单选：**真的填了内容**才让预设选中让位（互斥）——
                                            // 只是点一下输入框不打字不算作答，也不清掉已选预设
                                            if !multi && !cur.input.trim().is_empty() {
                                                cur.picked.clear();
                                            }
                                            states.write().insert(input_key.clone(), cur);
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── 底部导航：左「推荐选项」，右侧「取消 / 上一步 / 继续（末题为提交）」 ──
            div { class: Styles::wizard_nav,
                {
                    let reco_key = key.clone();
                    let no_reco = reco_idx.is_none();
                    rsx! {
                        Button {
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::Sm,
                            class: Styles::recommend_btn,
                            disabled: no_reco,
                            title: if no_reco { "本题没有推荐项" } else { "采纳推荐的选项" },
                            onclick: move |_| {
                                if let Some(ri) = reco_idx {
                                    apply_pick(states, current, &reco_key, ri, multi, is_last, options_len);
                                }
                            },
                            "✏️ 推荐选项"
                        }
                    }
                }
                div { class: Styles::wizard_nav_actions,
                    {
                        let on_submit = on_submit.clone();
                        rsx! {
                            Button {
                                variant: ButtonVariant::Ghost,
                                size: ButtonSize::Sm,
                                title: "取消本轮交互，整体跳过",
                                onclick: move |_| on_submit.call(ActionReply::Cancel),
                                "取消"
                            }
                        }
                    }
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
                        let tip = if !answered {
                            "请先作答"
                        } else if is_last {
                            "提交本批作答"
                        } else {
                            "进入下一题"
                        };
                        rsx! {
                            Button {
                                variant: ButtonVariant::Primary,
                                size: ButtonSize::Sm,
                                disabled: !answered,
                                title: "{tip}",
                                onclick: move |_| {
                                    if is_last {
                                        // 末题：打包整批回传
                                        let map = states.read();
                                        let choice = build_choice(&questions, &map);
                                        on_submit.call(ActionReply::Submit(choice));
                                    } else {
                                        current.set(qi + 1);
                                    }
                                },
                                "继续"
                            }
                        }
                    }
                }
            }
        }
    }
}
