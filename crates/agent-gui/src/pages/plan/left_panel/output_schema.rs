//! OUTPUT Bento 块：输出契约（`output_schema`）。
//!
//! 契约描述「这次执行要交出什么」，由输出定义步（`flexible_output`）与用户确认后定稿，
//! 随模板落库（`plans_flexible_sessions.parameterized_task`）。
//!
//! 展示是**宽容**的：契约是 LLM 生成的，任何形态都不该让面板崩 —— 解析失败降级成一句
//! 可读的提示（而不是空块或 panic）。
//!
//! 契约可以为 `null`：用户跳过了输出定义步，或明确表示「现在还定不了」。这两种情形
//! 语义等同（见 `docs/planned-agent/flexible-output-step.md` §5），这里统一显示空态。

use dioxus::prelude::*;
use planned_agent::flexible::{OutputKind, OutputSchema};
use serde_json::Value;

#[component]
pub fn OutputSchemaView(
    /// 模板里的原始契约 JSON；`None` = 没定义（跳过 / 定不了）。
    schema: Option<Value>,
    /// 模板未就绪时的提示（优先于契约本身）。
    hint: Option<String>,
) -> Element {
    // 先把「要展示什么」算清楚：空态文案 or 解析好的契约。
    let body = match (hint, schema) {
        (Some(hint), _) => Body::Empty(hint),
        (None, None) => Body::Empty("未定义输出（可在对话里说「定义输出」）".to_string()),
        (None, Some(raw)) => match OutputSchema::parse(&raw) {
            Ok(parsed) => Body::Schema(parsed),
            // 旧结构（`description` / `kind: success_only`）会落到这里：如实说清哪里不对
            Err(reason) => Body::Empty(format!("输出契约无效：{reason}")),
        },
    };

    rsx! {
        div { class: "plan-bento-block",
            div { class: "plan-bento-block__header",
                span { class: "plan-bento-block__header-emoji", "📤" }
                span { class: "plan-bento-block__header-label", "OUTPUT" }
            }
            div { class: "plan-bento-block__body",
                match body {
                    Body::Empty(text) => rsx! {
                        div { class: "plan-bento-empty", "{text}" }
                    },
                    Body::Schema(schema) => rsx! {
                        OutputSchemaBody { schema: schema }
                    },
                }
            }
        }
    }
}

/// 块体要展示的东西：空态文案，或解析好的契约。
enum Body {
    Empty(String),
    Schema(OutputSchema),
}

/// 契约正文：形态徽标 + 要交付什么 + 成功判据 + 形态要求 + 字段清单。
#[component]
fn OutputSchemaBody(schema: OutputSchema) -> Element {
    let kind = kind_label(schema.kind);
    let goal = schema.goal.clone();
    let success = schema.success.clone();
    let format = schema.format.clone();
    let required = schema.required.clone();
    let wanted = schema.wanted.clone();

    rsx! {
        div { class: "plan-output__kind-row",
            span { class: "plan-output__kind", "{kind}" }
        }
        if let Some(goal) = goal {
            div { class: "plan-output__line", "{goal}" }
        }
        if let Some(success) = success {
            div { class: "plan-output__line plan-output__line--muted", "成功判据：{success}" }
        }
        if let Some(format) = format {
            div { class: "plan-output__line plan-output__line--muted", "形态：{format}" }
        }
        if !required.is_empty() {
            div { class: "plan-output__fields",
                span { class: "plan-output__fields-label", "必须有" }
                for name in required {
                    span { class: "plan-output__chip plan-output__chip--required", "{name}" }
                }
            }
        }
        if !wanted.is_empty() {
            div { class: "plan-output__fields",
                span { class: "plan-output__fields-label", "尽力找" }
                for name in wanted {
                    span { class: "plan-output__chip", "{name}" }
                }
            }
        }
    }
}

/// 结果形态的中文名（与 `flexible_output.toml` 的六个取值一一对应）。
fn kind_label(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Bool => "只要成功/失败",
        OutputKind::Text => "文本",
        OutputKind::Markdown => "Markdown 文档",
        OutputKind::Json => "JSON 对象",
        OutputKind::Csv => "CSV 表格",
        OutputKind::File => "落盘文件",
    }
}
