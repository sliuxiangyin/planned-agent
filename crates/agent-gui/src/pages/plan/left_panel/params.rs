//! PARAMS Bento 块：当前会话参数化模板的参数定义。
//!
//! 数据来自 `plans_flexible_sessions.parameterized_task` 的 `inputs[]`
//! （由 `PlanLeftPanel` 的 resource 按当前会话读入）。
//! 参数值暂以**只读**呈现：可编辑与「执行时传参」属于执行接线，见
//! `docs/planned-agent/flexible-executor.md` 阶段 5。

use dioxus::prelude::*;
use planned_agent::flexible::PlanInput;
use serde_json::Value;

#[component]
pub fn ParamsView(inputs: Vec<PlanInput>, hint: Option<String>) -> Element {
    // 模板未就绪（hint 有值）→ 显示其原因；就绪但无参数 → 空态。
    // 只有这两种情况才不渲染参数行。
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
                                    class: "plan-params__input plan-params__input--readonly",
                                    readonly: true,
                                    value: "{param_display(input)}",
                                    title: "{input.description.clone().unwrap_or_default()}",
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 参数默认值的展示文本：字符串直出，其它 JSON 走紧凑序列化，无默认值标 `—`。
fn param_display(input: &PlanInput) -> String {
    match &input.default {
        Some(Value::String(s)) => s.clone(),
        Some(value) => value.to_string(),
        None => "—".to_string(),
    }
}
