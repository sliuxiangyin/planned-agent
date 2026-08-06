//! 创建计划弹窗：基于本地 dialog 组件的表单弹窗（名称输入 + 模式选择）。

use dioxus::prelude::*;

use crate::components::button::{Button, ButtonSize, ButtonVariant};
use crate::components::dialog::{Dialog, DialogDescription, DialogTitle};
use crate::components::input::Input;
use crate::components::select::{Select, SelectOption};

#[css_module("/src/pages/home/components/create_plan_modal/style.css")]
struct Styles;

/// 创建计划时收集的数据
#[derive(Clone, Debug)]
pub struct CreatePlanData {
    pub name: String,
    pub mode: String,
}

#[component]
pub fn CreatePlanModal(
    is_open: bool,
    on_close: EventHandler<()>,
    on_confirm: EventHandler<CreatePlanData>,
) -> Element {
    let mut name = use_signal(String::new);
    let mut mode = use_signal(|| "flexible".to_string());
    let mut error = use_signal(|| None::<String>);

    // 关键策略：dioxus_primitives::dialog 的 `use_outside_dismiss` 与 `use_global_escape_listener`
    // 都会调用 `set_open(false)`，进而触发 `on_open_change(false)`（参见 use_controlled 实现）。
    // 这里只响应 true（打开由父组件驱动），故意忽略所有 false：
    //   - 点击 backdrop / DialogContent 外部 → 不关闭
    //   - 按 ESC → 不关闭
    //   - 取消/确定按钮直接调用 props.on_close，让父组件 set is_open = false → Dialog 关闭
    let handle_open_change = move |open: bool| {
        if open {
            // 打开请求由父组件统一管理，此处无操作
            return;
        }
        // 关闭请求一律忽略，强制只能通过按钮关闭
    };

    let handle_confirm = move |_: MouseEvent| {
        let n = name.read().trim().to_string();
        if n.is_empty() {
            error.set(Some("计划名称不能为空".into()));
            return;
        }
        let m = mode.read().clone();
        on_confirm.call(CreatePlanData { name: n, mode: m });
        name.set(String::new());
        mode.set("flexible".to_string());
        error.set(None);
        on_close.call(());
    };

    let handle_cancel = move |_: MouseEvent| {
        name.set(String::new());
        mode.set("flexible".to_string());
        error.set(None);
        on_close.call(());
    };

    rsx! {
        Dialog {
            open: is_open,
            on_open_change: handle_open_change,
            DialogTitle {
                "新建计划"
            }
            DialogDescription {
                class: Styles::create_plan_dialog__body,
                // 计划名称
                div { class: Styles::create_plan_dialog__field,
                    label { class: Styles::create_plan_dialog__label, "计划名称" }
                    Input {
                        placeholder: "输入计划名称...",
                        value: "{name}",
                        oninput: move |e: FormEvent| {
                            name.set(e.value());
                            error.set(None);
                        },
                    }
                }
                // 错误提示
                if let Some(ref err) = *error.read() {
                    div { class: Styles::create_plan_dialog__error, "{err}" }
                }
                // 计划模式
                div { class: Styles::create_plan_dialog__field,
                    label { class: Styles::create_plan_dialog__label, "计划模式" }
                    Select::<String> {
                        default_value: "flexible".to_string(),
                        on_value_change: move |val: Option<String>| {
                            if let Some(v) = val {
                                mode.set(v);
                            }
                        },
                        SelectOption::<String> {
                            index: 0usize,
                            value: "flexible".to_string(),
                            text_value: "灵活模式".to_string(),
                            "灵活模式"
                        }
                        SelectOption::<String> {
                            index: 1usize,
                            value: "thorough".to_string(),
                            text_value: "周密模式".to_string(),
                            "周密模式"
                        }
                    }
                }
            }
            // 操作按钮（Dialog 的 children 直接进入 DialogContent，与 DialogTitle/DialogDescription 同级）
            div { class: Styles::create_plan_dialog__footer,
                Button {
                    variant: ButtonVariant::Outline,
                    size: ButtonSize::Sm,
                    onclick: handle_cancel,
                    "取消"
                }
                Button {
                    variant: ButtonVariant::Primary,
                    size: ButtonSize::Sm,
                    onclick: handle_confirm,
                    "确定"
                }
            }
        }
    }
}