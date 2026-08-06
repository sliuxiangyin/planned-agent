use crate::components::button::{Button, ButtonSize, ButtonVariant};
use dioxus::prelude::*;
use dioxus_primitives::accordion::{
    self, AccordionContentProps, AccordionItemProps, AccordionProps, AccordionTriggerProps,
};
use dioxus_primitives::{dioxus_attributes::attributes, merge_attributes};

#[allow(dead_code)]
type _PropsRef = (AccordionProps, AccordionItemProps, AccordionTriggerProps, AccordionContentProps);

#[css_module("/src/components/todo/style.css")]
struct Styles;

/// 计划项的执行状态。
///
/// 本组件只负责 UI 渲染，状态字段仅作为外观区分的依据，
/// 不参与实际的执行控制逻辑。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TodoStatus {
    /// 未开始：空心圆环
    Pending,
    /// 待运行：实心圆点
    Queued,
    /// 运行中：旋转圆环
    Running,
    /// 已完成：勾选
    Completed,
    /// 失败：叉号
    Failed,
    /// 已跳过：横线
    Skipped,
}

/// 单个计划项的展示数据（纯 UI 数据模型）。
#[derive(Clone, PartialEq)]
pub struct TodoItemData {
    pub status: TodoStatus,
    pub title: String,
    /// 收起时省略的标题；展开后展示完整描述。
    pub detail: String,
}

impl TodoItemData {
    pub fn new(status: TodoStatus, title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            status,
            title: title.into(),
            detail: detail.into(),
        }
    }
}

/// 状态图标（内联 SVG）。统一 16x16，颜色由 CSS 控制。
fn status_icon(status: TodoStatus) -> Element {
    let base = Styles::dx_todo__status_icon;
    let modifier = match status {
        TodoStatus::Pending => Styles::dx_todo__status_icon__pending,
        TodoStatus::Queued => Styles::dx_todo__status_icon__queued,
        TodoStatus::Running => Styles::dx_todo__status_icon__running,
        TodoStatus::Completed => Styles::dx_todo__status_icon__completed,
        TodoStatus::Failed => Styles::dx_todo__status_icon__failed,
        TodoStatus::Skipped => Styles::dx_todo__status_icon__skipped,
    };
    let class = format!("{} {}", base, modifier);
    match status {
        TodoStatus::Pending => rsx! {
            svg {
                class: "{class}",
                view_box: "0 0 16 16",
                width: "16",
                height: "16",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "1.5",
                circle { cx: "8", cy: "8", r: "6" }
            }
        },
        TodoStatus::Queued => rsx! {
            svg {
                class: "{class}",
                view_box: "0 0 16 16",
                width: "16",
                height: "16",
                fill: "currentColor",
                circle { cx: "8", cy: "8", r: "4" }
            }
        },
        TodoStatus::Running => rsx! {
            svg {
                class: "{class}",
                view_box: "0 0 16 16",
                width: "16",
                height: "16",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "1.75",
                stroke_linecap: "round",
                path { d: "M 8 2 A 6 6 0 1 1 2 8" }
            }
        },
        TodoStatus::Completed => rsx! {
            svg {
                class: "{class}",
                view_box: "0 0 16 16",
                width: "16",
                height: "16",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "1.75",
                stroke_linecap: "round",
                stroke_linejoin: "round",
                circle { cx: "8", cy: "8", r: "6" }
                path { d: "M 5 8.5 L 7 10.5 L 11 6" }
            }
        },
        TodoStatus::Failed => rsx! {
            svg {
                class: "{class}",
                view_box: "0 0 16 16",
                width: "16",
                height: "16",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "1.75",
                stroke_linecap: "round",
                circle { cx: "8", cy: "8", r: "6" }
                path { d: "M 5.5 5.5 L 10.5 10.5" }
                path { d: "M 10.5 5.5 L 5.5 10.5" }
            }
        },
        TodoStatus::Skipped => rsx! {
            svg {
                class: "{class}",
                view_box: "0 0 16 16",
                width: "16",
                height: "16",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "1.5",
                circle { cx: "8", cy: "8", r: "6" }
                path { d: "M 4 8 L 12 8" }
            }
        },
    }
}

/// Todo 面板根组件：渲染为浮起的 Card，默认仅显示 header（折叠态）。
///
/// - 整体高度自适应：展开时由 body 自然撑开（最大 320px 滚动），折叠时仅显示 header
/// - `data-expanded="true|false"` 暴露给 CSS 用于 chevron 旋转
/// - 当前仅渲染 UI，不接入真实数据；`items` 为空时显示占位提示
#[component]
pub fn Todo(
    items: Vec<TodoItemData>,
    #[props(default)] on_execute: Option<EventHandler<MouseEvent>>,
    /// 外部强制展开触发器：值递增时强制展开面板（如计划生成/保存完成）。
    /// 用户手动折叠后，若该值不变则保持折叠。
    #[props(default)]
    expand_token: Signal<u32, SyncStorage>,
) -> Element {
    let count = items.len();
    // 整体折叠/展开：默认 false（仅显示 header）
    let mut is_open = use_signal(|| false);

    // 外部触发强制展开：expand_token 递增时重新展开
    use_effect(move || {
        if *expand_token.read() > 0 {
            is_open.set(true);
        }
    });

    let is_open_val = is_open();

    // 运行状态：控制开始/暂停图标的切换
    let mut is_running = use_signal(|| false);

    // 播放图标
    let play_icon = rsx! {
        svg {
            view_box: "0 0 16 16",
            width: "14",
            height: "14",
            fill: "currentColor",
            path { d: "M 4 2.5 L 13 8 L 4 13.5 Z" }
        }
    };

    // 暂停图标
    let pause_icon = rsx! {
        svg {
            view_box: "0 0 16 16",
            width: "14",
            height: "14",
            fill: "currentColor",
            rect { x: "3", y: "2.5", width: "3.5", height: "11" }
            rect { x: "9.5", y: "2.5", width: "3.5", height: "11" }
        }
    };

    rsx! {
        div {
            class: Styles::dx_todo,
            "data-expanded": if is_open_val { "true" } else { "false" },

            // ── Header：左侧标题组 + 右侧操作组，flexbox 垂直居中 ──
            div {
                class: Styles::dx_todo__header,
                div {
                    class: Styles::dx_todo__title,
                    span { class: Styles::dx_todo__title_text, "执行计划" }
                    span { class: Styles::dx_todo__count,
                        if count == 0 {
                            "0 项"
                        } else {
                            "{count} 项"
                        }
                    }
                }
                div {
                    class: Styles::dx_todo__actions,
                    Button {
                        class: Styles::dx_todo__run_btn,
                        variant: if *is_running.read() { ButtonVariant::Destructive } else { ButtonVariant::Primary },
                        size: ButtonSize::IconXs,
                        disabled: items.is_empty(),
                        title: if *is_running.read() { "暂停执行" } else { "开始执行" },
                        aria_label: if *is_running.read() { "暂停执行" } else { "开始执行" },
                        onclick: move |e| {
                            is_running.toggle();
                            if let Some(f) = &on_execute {
                                f.call(e);
                            }
                        },
                        if *is_running.read() { {pause_icon} } else { {play_icon} }
                    }
                    Button {
                        class: Styles::dx_todo__collapse_btn,
                        variant: ButtonVariant::Ghost,
                        title: if is_open_val { "折叠计划" } else { "展开计划" },
                        aria_label: if is_open_val { "折叠计划" } else { "展开计划" },
                        onclick: move |_| {
                            is_open.with_mut(|v| *v = !*v);
                        },
                        svg {
                            class: Styles::dx_todo__collapse_icon,
                            view_box: "0 0 16 16",
                            width: "14",
                            height: "14",
                            fill: "none",
                            stroke: "currentColor",
                            stroke_width: "1.75",
                            stroke_linecap: "round",
                            stroke_linejoin: "round",
                            path { d: "M 4 6 L 8 10 L 12 6" }
                        }
                    }
                }
            }

            // ── Body：默认不渲染（折叠态）；展开时挂载列表/空状态 ──
            if is_open_val {
                div {
                    class: Styles::dx_todo__body,
                    if items.is_empty() {
                        div { class: Styles::dx_todo__empty,
                            p { class: Styles::dx_todo__empty_title, "暂无执行计划" }
                            p { class: Styles::dx_todo__empty_hint, "AI 生成计划后将在这里显示" }
                        }
                    } else {
                        div { class: Styles::dx_todo__list,
                            accordion::Accordion {
                                class: Styles::dx_todo__accordion,
                                allow_multiple_open: true,
                                collapsible: true,
                                horizontal: false,
                                for (idx, item) in items.iter().enumerate() {
                                    TodoItem {
                                        index: idx,
                                        data: item.clone(),
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

/// 单个计划项。直接使用 `dioxus_primitives::accordion` 原语：
/// - 拥有 ARIA / 键盘导航 / 焦点管理 / 自动 open-close 状态
/// - 视觉与高度由组件自身样式控制，避免 styled Accordion 的固定高度限制
#[component]
fn TodoItem(index: usize, data: TodoItemData) -> Element {
    rsx! {
        AccordionItemShell {
            class: Styles::dx_todo__item,
            index: index,
            AccordionTriggerShell {
                span { class: Styles::dx_todo__trigger_row,
                    {status_icon(data.status)}
                    span { class: Styles::dx_todo__item_title, "{data.title}" }
                    svg {
                        class: Styles::dx_todo__chevron,
                        view_box: "0 0 16 16",
                        width: "14",
                        height: "14",
                        fill: "none",
                        stroke: "currentColor",
                        stroke_width: "1.75",
                        stroke_linecap: "round",
                        stroke_linejoin: "round",
                        path { d: "M 4 6 L 8 10 L 12 6" }
                    }
                }
            }
            AccordionContentShell {
                div { class: Styles::dx_todo__detail, "{data.detail}" }
            }
        }
    }
}

// ── Accordion 原语薄壳：合并 class + 把自身属性透传下去 ─────────────────

#[component]
fn AccordionItemShell(
    class: String,
    index: usize,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let extra = attributes!(div { class: "{class}" });
    let merged = merge_attributes(vec![extra, attributes]);
    rsx! {
        accordion::AccordionItem {
            index: index,
            attributes: merged,
            {children}
        }
    }
}

#[component]
fn AccordionTriggerShell(
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let extra = attributes!(button { class: Styles::dx_todo_trigger });
    let merged = merge_attributes(vec![extra, attributes]);
    rsx! {
        accordion::AccordionTrigger {
            attributes: merged,
            {children}
        }
    }
}

#[component]
fn AccordionContentShell(
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let extra = attributes!(div { class: Styles::dx_todo_content });
    let merged = merge_attributes(vec![extra, attributes]);
    rsx! {
        accordion::AccordionContent {
            attributes: merged,
            {children}
        }
    }
}
