//! 快捷操作栏：语音输入、新建计划、一键执行。

use dioxus::prelude::*;

#[css_module("/src/pages/home/components/quick_actions/style.css")]
struct Styles;

#[component]
pub fn QuickActionsBar(
    on_new_plan: EventHandler<MouseEvent>,
    on_execute_all: EventHandler<MouseEvent>,
    on_voice_input: EventHandler<MouseEvent>,
) -> Element {
    rsx! {
        div { class: Styles::cc_quick_actions,
            // 语音输入区（主打：对话式创建）
            button {
                class: "{Styles::cc_action_btn} {Styles::cc_action_btn__voice}",
                onclick: move |e| on_voice_input.call(e),
                span { class: Styles::cc_action_btn__icon, "🎤" }
                span { class: Styles::cc_action_btn__label, "对 Agent 说..." }
                span { class: Styles::cc_action_btn__hint, "语音创建计划" }
            }

            div { class: Styles::cc_quick_actions__divider }

            // 新建计划
            button {
                class: "{Styles::cc_action_btn} {Styles::cc_action_btn__primary}",
                onclick: move |e| on_new_plan.call(e),
                span { class: Styles::cc_action_btn__icon, "＋" }
                span { class: Styles::cc_action_btn__label, "新建计划" }
                span { class: Styles::cc_action_btn__hint, "Ctrl+N" }
            }

            // 一键执行
            button {
                class: "{Styles::cc_action_btn} {Styles::cc_action_btn__execute}",
                onclick: move |e| on_execute_all.call(e),
                span { class: Styles::cc_action_btn__icon, "▶" }
                span { class: Styles::cc_action_btn__label, "全部执行" }
                span { class: Styles::cc_action_btn__hint, "执行所有排队计划" }
            }

            div { class: Styles::cc_quick_actions__divider }

            // 预留按钮
            button {
                class: "{Styles::cc_action_btn} {Styles::cc_action_btn__reserved}",
                disabled: true,
                title: "定时执行 — 即将上线",
                span { class: Styles::cc_action_btn__icon, "⏰" }
                span { class: Styles::cc_action_btn__label, "定时" }
            }
            button {
                class: "{Styles::cc_action_btn} {Styles::cc_action_btn__reserved}",
                disabled: true,
                title: "多策略执行 — 即将上线",
                span { class: Styles::cc_action_btn__icon, "🔗" }
                span { class: Styles::cc_action_btn__label, "策略" }
            }
        }
    }
}