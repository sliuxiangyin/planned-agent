use dioxus::prelude::*;

#[css_module("/src/components/page_header/style.css")]
struct Styles;

/// 通用页面顶栏组件
///
/// ## 设计要点
///
/// - **三段式布局**：返回按钮（可选） + 标题（必填） + 右侧操作槽位（可选）
/// - **变体策略**：默认 48px 高度；通过传入 `class: "dx-page-header--nested"` 切换为 40px 嵌入版
/// - **API 灵活**：`on_back` / `actions` / `back_label` / `back_disabled` 均为 Option，调用方按需组合
///
/// ## Props
///
/// - `title`：页面标题（h1）
/// - `on_back`：返回按钮回调；`None` = 不渲染返回按钮
/// - `back_label`：返回按钮文字，默认 `"← 返回"`
/// - `back_disabled`：返回按钮禁用态（保存中等异步场景）
/// - `actions`：右侧操作区（保存、删除等按钮）；`None` = 不渲染右侧槽位
/// - `class`：调用方自定义 class，与默认 class 共存（用于覆盖默认外观）
#[component]
pub fn PageHeader(
    title: String,
    #[props(default)]
    on_back: Option<EventHandler<()>>,
    #[props(default)]
    back_label: Option<String>,
    #[props(default)]
    back_disabled: Option<bool>,
    #[props(default)]
    actions: Option<Element>,
    #[props(default)]
    class: Option<String>,
) -> Element {
    // 默认 class（合并 dx_page_header + 调用方传入的 class）
    let mut default_class = format!("{} {}", Styles::dx_page_header, Styles::dx_page_header_back_title_actions);
    if let Some(extra) = class.as_ref() {
        if !extra.is_empty() {
            default_class.push(' ');
            default_class.push_str(extra);
        }
    }

    // 返回按钮文字（默认 ← 返回）
    let back_text = back_label
        .unwrap_or_else(|| "← 返回".to_string());

    // 返回按钮是否禁用
    let is_back_disabled = back_disabled.unwrap_or(false);

    rsx! {
        header {
            class: "{default_class}",
            "data-slot": "page-header",

            // 返回按钮（仅当 on_back 存在）
            if let Some(on_back) = on_back {
                {
                    let back_text_for_btn = back_text.clone();
                    rsx! {
                        button {
                            class: Styles::dx_page_header_back,
                            "data-slot": "page-header-back",
                            disabled: is_back_disabled,
                            onclick: move |_| {
                                if !is_back_disabled {
                                    on_back.call(());
                                }
                            },
                            "{back_text_for_btn}"
                        }
                    }
                }
            }

            // 标题（必填）
            h1 {
                class: Styles::dx_page_header_title,
                "data-slot": "page-header-title",
                "{title}"
            }

            // 右侧操作槽位（仅当 actions 存在）
            if let Some(actions) = actions {
                div {
                    class: Styles::dx_page_header_actions,
                    "data-slot": "page-header-actions",
                    {actions}
                }
            }
        }
    }
}