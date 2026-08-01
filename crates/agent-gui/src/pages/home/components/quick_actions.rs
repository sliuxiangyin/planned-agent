//! 快捷操作栏：语音输入、新建计划、一键执行。

use dioxus::prelude::*;

#[component]
pub fn QuickActionsBar(
    on_new_plan: EventHandler<MouseEvent>,
    on_execute_all: EventHandler<MouseEvent>,
    on_voice_input: EventHandler<MouseEvent>,
) -> Element {
    rsx! {
        div { class: "cc-quick-actions",
            // 语音输入区（主打：对话式创建）
            button {
                class: "cc-action-btn cc-action-btn--voice",
                onclick: move |e| on_voice_input.call(e),
                span { class: "cc-action-btn__icon", "🎤" }
                span { class: "cc-action-btn__label", "对 Agent 说..." }
                span { class: "cc-action-btn__hint", "语音创建计划" }
            }

            div { class: "cc-quick-actions__divider" }

            // 新建计划
            button {
                class: "cc-action-btn cc-action-btn--primary",
                onclick: move |e| on_new_plan.call(e),
                span { class: "cc-action-btn__icon", "＋" }
                span { class: "cc-action-btn__label", "新建计划" }
                span { class: "cc-action-btn__hint", "Ctrl+N" }
            }

            // 一键执行
            button {
                class: "cc-action-btn cc-action-btn--execute",
                onclick: move |e| on_execute_all.call(e),
                span { class: "cc-action-btn__icon", "▶" }
                span { class: "cc-action-btn__label", "全部执行" }
                span { class: "cc-action-btn__hint", "执行所有排队计划" }
            }

            div { class: "cc-quick-actions__divider" }

            // 预留按钮
            button {
                class: "cc-action-btn cc-action-btn--reserved",
                disabled: true,
                title: "定时执行 — 即将上线",
                span { class: "cc-action-btn__icon", "⏰" }
                span { class: "cc-action-btn__label", "定时" }
            }
            button {
                class: "cc-action-btn cc-action-btn--reserved",
                disabled: true,
                title: "多策略执行 — 即将上线",
                span { class: "cc-action-btn__icon", "🔗" }
                span { class: "cc-action-btn__label", "策略" }
            }
        }
    }
}
