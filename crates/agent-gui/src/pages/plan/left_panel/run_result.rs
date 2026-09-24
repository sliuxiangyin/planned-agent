//! RESULT Bento 块：本次执行的最终结果。
//!
//! 结果由执行器末尾的**输出整理步**按契约整理（见 `docs/planned-agent/flexible-output-step.md`），
//! 经 `RunSnapshot::result` 上来。模板没有契约时，执行器退化为「交付步输出原文」，同样显示在这里。
//!
//! 三种「没有结果」的成因文案必须分开：没跑过 / 跑失败 / 没定义契约 —— 混成一句「暂无结果」
//! 会让用户不知道该去改什么。

use dioxus::prelude::*;
use planned_agent::flexible::run_service::RunStatus;

/// 块体要展示的东西：一段提示文案，或真正的结果。
enum Body {
    /// 提示（空态 / 执行中 / 无结果的原因）。
    Message(String),
    /// 结果正文。
    Result(String),
}

#[component]
pub fn RunResultView(
    /// 模板未就绪时的提示（优先于执行状态）。
    hint: Option<String>,
    /// 本次执行是否在进行中。
    is_running: bool,
    /// 本次执行的终态（未跑过时为 `None`）。
    status: Option<RunStatus>,
    /// 执行器算出的最终结果。
    result: Option<String>,
) -> Element {
    let body = if let Some(hint) = hint {
        Body::Message(hint)
    } else if is_running {
        Body::Message("执行中…".to_string())
    } else {
        match (result, status) {
            (Some(result), _) => Body::Result(result),
            // 没有结果时把成因说清楚：失败 / 取消 / 没定义契约，三者的下一步动作完全不同
            (None, Some(RunStatus::Failed)) => {
                Body::Message("执行未成功，没有最终结果（失败原因见 PIPELINE）".to_string())
            }
            (None, Some(RunStatus::Cancelled)) => Body::Message("已取消，没有最终结果".to_string()),
            (None, Some(RunStatus::Succeeded)) => {
                Body::Message("本次没有定义输出契约，结果见各步骤的产出".to_string())
            }
            // `is_running` 与 `status` 取自同一份快照，这里只是把 Running 显式列出来（不可达）；
            // 其余（含未跑过的 `None`）统一按「还没执行」呈现。
            (None, Some(RunStatus::Running)) => Body::Message("执行中…".to_string()),
            (None, _) => Body::Message("尚未执行（在 PIPELINE 底部点「执行」）".to_string()),
        }
    };

    rsx! {
        div { class: "plan-bento-block",
            div { class: "plan-bento-block__header",
                span { class: "plan-bento-block__header-emoji", "🎁" }
                span { class: "plan-bento-block__header-label", "RESULT" }
            }
            div { class: "plan-bento-block__body",
                match body {
                    Body::Message(text) => rsx! {
                        div { class: "plan-bento-empty", "{text}" }
                    },
                    Body::Result(text) => rsx! {
                        pre { class: "plan-run-result__text", "{text}" }
                    },
                }
            }
        }
    }
}
