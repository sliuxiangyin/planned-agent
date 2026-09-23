//! PARAMS Bento 块：当前会话参数化模板的参数定义。
//!
//! 数据来自 `plans_flexible_sessions.parameterized_task` 的 `inputs[]`
//! （由 `PlanLeftPanel` 的 resource 按当前会话读入）。
//!
//! 输入框**可编辑**，初值是该参数的 `default`；编辑态由容器持有（`values`），
//! 因为 PIPELINE 的占位符展开也要读同一份值。

use std::collections::BTreeMap;

use dioxus::prelude::*;
use planned_agent::flexible::PlanInput;

use super::left_panel::value_text;

/// 输入框当前应显示的值：用户覆盖优先，否则回落模板默认值。
///
/// 渲染期就兜底，而不是等 effect 把默认值写进 signal —— 否则模板就绪那一帧输入框会空一下。
fn current_text(values: &Signal<BTreeMap<String, String>>, input: &PlanInput) -> String {
    values
        .read()
        .get(&input.name)
        .cloned()
        .unwrap_or_else(|| input.default.as_ref().map(value_text).unwrap_or_default())
}

#[component]
pub fn ParamsView(
    inputs: Vec<PlanInput>,
    // 参数编辑态（参数名 → 输入框文本），由 `PlanLeftPanel` 持有以便 PIPELINE 共享。
    values: Signal<BTreeMap<String, String>>,
    hint: Option<String>,
) -> Element {
    // 模板未就绪（hint 有值）→ 显示其原因；就绪但无参数 → 空态。
    let empty_hint = hint.or_else(|| inputs.is_empty().then(|| "该模板未定义参数".to_string()));

    rsx! {
        div { class: "plan-bento-block",
            div { class: "plan-bento-block__header",
                span { class: "plan-bento-block__header-emoji", "🔧" }
                span { class: "plan-bento-block__header-label", "PARAMS" }
            }
            div { class: "plan-bento-block__body",
                if let Some(hint) = empty_hint {
                    div { class: "plan-bento-empty", "{hint}" }
                } else {
                    for input in inputs.iter() {
                        div { class: "plan-params__item", key: "{input.name}",
                            span { class: "plan-params__label", "{input.name}" }
                            div { class: "plan-params__input-wrap",
                                input {
                                    class: "plan-params__input",
                                    value: "{current_text(&values, input)}",
                                    // 清空后仍能看到原本的默认值，便于对照
                                    placeholder: "{input.default.as_ref().map(value_text).unwrap_or_default()}",
                                    title: "{input.description.clone().unwrap_or_default()}",
                                    oninput: {
                                        let name = input.name.clone();
                                        move |event: FormEvent| {
                                            values.write().insert(name.clone(), event.value());
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
