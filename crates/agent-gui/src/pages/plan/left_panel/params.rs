//! PARAMS Bento 块：已固化的计划参数定义。
//!
//! 展示 `target_url` / `max_pages` / `output_format` 三个参数项，
//! 其中 `output_format` 带编辑按钮。当前为静态 mock，
//! 后续接入 `plan_params` 信号渲染真实参数。

use dioxus::prelude::*;

#[component]
pub fn ParamsView() -> Element {
    rsx! {
        div { class: "plan-bento-block",
            div { class: "plan-bento-block__header",
                span { class: "plan-bento-block__header-emoji", "🔧" }
                span { class: "plan-bento-block__header-label", "PARAMS" }
            }
            div { class: "plan-bento-block__body",
                div { class: "plan-params__item",
                    span { class: "plan-params__label", "target_url" }
                    div { class: "plan-params__input-wrap",
                        input {
                            class: "plan-params__input plan-params__input--readonly",
                            value: "https://example.com/api/v2",
                            readonly: true,
                        }
                    }
                }
                div { class: "plan-params__item",
                    span { class: "plan-params__label", "max_pages" }
                    div { class: "plan-params__input-wrap",
                        input {
                            class: "plan-params__input plan-params__input--readonly",
                            value: "5",
                            readonly: true,
                        }
                    }
                }
                div { class: "plan-params__item",
                    span { class: "plan-params__label", "output_format" }
                    div { class: "plan-params__input-wrap",
                        input {
                            class: "plan-params__input plan-params__input--readonly",
                            value: "markdown",
                            readonly: true,
                        }
                        button {
                            class: "plan-params__edit-btn",
                            title: "编辑参数",
                            svg {
                                xmlns: "http://www.w3.org/2000/svg",
                                width: "13",
                                height: "13",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "2",
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                path { d: "M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" }
                                path { d: "m15 5 4 4" }
                            }
                        }
                    }
                }
            }
        }
    }
}
