use crate::components::separator::Separator;
use dioxus::prelude::*;

/// 可拖拽调整左右宽度的分栏布局组件。
///
/// ## Props
/// - `left` / `right`: 左右面板内容
/// - `initial_left_percent`: 左侧初始宽度百分比（默认 70）
/// - `min_left_percent` / `max_left_percent`: 拖拽范围限制
#[component]
pub fn ResizablePanel(
    /// 左侧面板内容
    left: Element,
    /// 右侧面板内容
    right: Element,
    /// 左侧初始宽度百分比
    #[props(default = 70.0)]
    initial_left_percent: f64,
    /// 左侧最小宽度百分比
    #[props(default = 25.0)]
    min_left_percent: f64,
    /// 左侧最大宽度百分比
    #[props(default = 75.0)]
    max_left_percent: f64,
) -> Element {
    // ── 拖拽状态 ──
    let mut split_percent = use_signal(|| initial_left_percent);
    let mut is_dragging = use_signal(|| false);
    let mut drag_start_x = use_signal(|| 0.0f64);
    let mut drag_start_percent = use_signal(|| initial_left_percent);
    let mut viewport_width = use_signal(|| 1024.0f64);

    // 获取视口宽度（像素 → 百分比转换）
    use_effect(move || {
        spawn(async move {
            let result = document::eval("window.innerWidth").await;
            if let Ok(value) = result {
                if let Some(w) = value.as_f64() {
                    viewport_width.set(w);
                }
            }
        });
    });

    // ── 拖拽事件 ──
    let on_divider_mousedown = move |e: MouseEvent| {
        is_dragging.set(true);
        drag_start_x.set(e.data.client_coordinates().x);
        drag_start_percent.set(split_percent());
    };

    let on_overlay_mousemove = move |e: MouseEvent| {
        let dx = e.data.client_coordinates().x - drag_start_x();
        let dpct = (dx / viewport_width()) * 100.0;
        let new_pct = (drag_start_percent() + dpct).clamp(min_left_percent, max_left_percent);
        split_percent.set(new_pct);
    };

    let on_overlay_mouseup = move |_| {
        is_dragging.set(false);
    };

    let left_pct = split_percent();
    let is_dragging_class = if is_dragging() { "resizable-panel--dragging" } else { "" };

    rsx! {
        div {
            class: "resizable-panel {is_dragging_class}",
            "data-split": "{left_pct}",

            // ═══ 左侧面板 ═══
            div {
                class: "resizable-panel__left",
                style: "width: {left_pct}%;",
                {left}
            }

            // ═══ 可拖拽分割线 ═══
            div {
                class: "resizable-panel__divider",
                onmousedown: on_divider_mousedown,
                Separator {
                    horizontal: false,
                    decorative: true,
                }
            }

            // ═══ 右侧面板 ═══
            div {
                class: "resizable-panel__right",
                {right}
            }

            // ═══ 拖拽遮罩层 ═══
            if is_dragging() {
                div {
                    class: "resizable-panel__overlay",
                    onmousemove: on_overlay_mousemove,
                    onmouseup: on_overlay_mouseup,
                    onmouseleave: on_overlay_mouseup,
                }
            }
        }
    }
}
