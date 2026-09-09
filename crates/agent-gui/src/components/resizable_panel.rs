use crate::components::separator::Separator;
use dioxus::prelude::*;
use dioxus_icons::lucide::{ChevronsLeft, ChevronsRight};

/// 可拖拽调整三栏宽度的分栏布局组件：`left | center | right`。
///
/// 布局由两条分割线决定，两条线各自独立拖拽，只改变相邻两栏的比例：
/// - 拖 `left|center` 分割线：在 `left` 与 `center` 之间重分配，`right` 不动；
/// - 拖 `center|right` 分割线：在 `center` 与 `right` 之间重分配，`left` 不动。
///
/// 各栏宽度取整百分比：`left = d1`，`center = d2 - d1`，`right = 100 - d2`，
/// 其中 `d1` / `d2` 分别为两条分割线相对容器左缘的位置。
///
/// ## Props
/// - `left` / `center` / `right`: 三栏面板内容
/// - `initial_left_percent` / `initial_center_percent`: 初始 left/center 宽度百分比
///   （right 自动取剩余 `100 - left - center`）
/// - `min_left_percent` / `min_center_percent` / `min_right_percent`: 各栏拖拽下限
#[component]
pub fn ResizablePanel(
    /// 左侧面板内容
    left: Element,
    /// 中间面板内容
    center: Element,
    /// 右侧面板内容
    right: Element,
    /// 左侧初始宽度百分比（默认 60）
    #[props(default = 60.0)]
    initial_left_percent: f64,
    /// 中间初始宽度百分比（默认 15，right 自动为 25）
    #[props(default = 15.0)]
    initial_center_percent: f64,
    /// 左侧最小宽度百分比
    #[props(default = 0.0)]
    min_left_percent: f64,
    /// 中间最小宽度百分比
    #[props(default = 0.0)]
    min_center_percent: f64,
    /// 右侧最小宽度百分比
    #[props(default = 0.0)]
    min_right_percent: f64,
    /// 侧栏过窄判定阈值：当 left（或 right）窄于该百分比时，
    /// 在最外缘浮现「点击恢复默认占比」的悬浮按钮，避免栏被贴边后无法再拖回。
    #[props(default = 15.0)]
    edge_recover_threshold_percent: f64,
) -> Element {
    // ── 分割线位置（百分比）：split1 = left|center 线（= left 宽度），
    //    split2 = center|right 线（= left + center 宽度） ──
    let mut split1 = use_signal(|| initial_left_percent);
    let mut split2 = use_signal(|| initial_left_percent + initial_center_percent);

    // ── 拖拽状态（每条线各自独立记录） ──
    let mut is_dragging = use_signal(|| false);
    let mut drag_start_x = use_signal(|| 0.0f64);
    let mut drag1_start_pct = use_signal(|| initial_left_percent);
    let mut drag2_start_pct = use_signal(|| initial_left_percent + initial_center_percent);
    let mut active_divider = use_signal(|| 0u8); // 1 = left|center，2 = center|right
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

    // ── 拖拽事件：按下分割线时记录起点与所属线 ──
    let on_divider1_mousedown = move |e: MouseEvent| {
        active_divider.set(1);
        is_dragging.set(true);
        drag_start_x.set(e.data.client_coordinates().x);
        drag1_start_pct.set(split1());
    };

    let on_divider2_mousedown = move |e: MouseEvent| {
        active_divider.set(2);
        is_dragging.set(true);
        drag_start_x.set(e.data.client_coordinates().x);
        drag2_start_pct.set(split2());
    };

    // ── 拖拽中：按所属线重分配相邻两栏，并守住各栏下限 ──
    let on_overlay_mousemove = move |e: MouseEvent| {
        let dx = e.data.client_coordinates().x - drag_start_x();
        let dpct = (dx / viewport_width()) * 100.0;

        match active_divider() {
            // 左|中 线：left 增 → center 减（right 不动）
            1 => {
                let max1 = split2() - min_center_percent;
                let new1 = (drag1_start_pct() + dpct).clamp(min_left_percent, max1);
                split1.set(new1);
            }
            // 中|右 线：center 增 → right 减（left 不动）
            _ => {
                let min2 = split1() + min_center_percent;
                let max2 = 100.0 - min_right_percent;
                let new2 = (drag2_start_pct() + dpct).clamp(min2, max2);
                split2.set(new2);
            }
        }
    };

    let on_overlay_mouseup = move |_| {
        is_dragging.set(false);
    };

    // ── 边缘悬浮按钮：恢复被贴边的侧栏到默认占比 ──
    // 左缘按钮 → 把 left 恢复到 initial_left_percent（向右挤出，挤占 center，触到其下限即停）
    let on_recover_left = move |_| {
        let max = split2() - min_center_percent;
        split1.set(initial_left_percent.clamp(min_left_percent, max));
    };
    // 右缘按钮 → 把 center|right 边界恢复到 initial_left+initial_center，使 right 回默认占比
    let on_recover_right = move |_| {
        let target = initial_left_percent + initial_center_percent;
        let min = split1() + min_center_percent;
        let max = 100.0 - min_right_percent;
        split2.set(target.clamp(min, max));
    };

    let s1 = split1();
    let s2 = split2();
    let center_pct = s2 - s1;
    let right_pct = 100.0 - s2;
    let left_narrow = s1 < edge_recover_threshold_percent;
    let right_narrow = right_pct < edge_recover_threshold_percent;
    let is_dragging_class = if is_dragging() { "resizable-panel--dragging" } else { "" };

    rsx! {
        div {
            class: "resizable-panel {is_dragging_class}",
            "data-split": "{s1}",

            // ═══ 左侧面板 ═══
            div {
                class: "resizable-panel__left",
                style: "width: {s1}%;",
                {left}
            }

            // ═══ 左|中 可拖拽分割线 ═══
            div {
                class: "resizable-panel__divider",
                onmousedown: on_divider1_mousedown,
                Separator {
                    horizontal: false,
                    decorative: true,
                }
            }

            // ═══ 中间面板 ═══
            div {
                class: "resizable-panel__center",
                style: "width: {center_pct}%;",
                {center}
            }

            // ═══ 中|右 可拖拽分割线 ═══
            div {
                class: "resizable-panel__divider",
                onmousedown: on_divider2_mousedown,
                Separator {
                    horizontal: false,
                    decorative: true,
                }
            }

            // ═══ 右侧面板 ═══
            div {
                class: "resizable-panel__right",
                style: "flex-basis: {right_pct}%;",
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

            // ═══ 边缘悬浮展开按钮：侧栏过窄无法拖回时浮现 ═══
            if left_narrow {
                div {
                    class: "resizable-panel__edge-btn resizable-panel__edge-btn--left",
                    title: "展开左栏",
                    onclick: on_recover_left,
                    ChevronsRight { size: "18" }
                }
            }
            if right_narrow {
                div {
                    class: "resizable-panel__edge-btn resizable-panel__edge-btn--right",
                    title: "展开右栏",
                    onclick: on_recover_right,
                    ChevronsLeft { size: "18" }
                }
            }
        }
    }
}
