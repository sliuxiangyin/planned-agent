//! 意图处理器
//!
//! 职责：把 `StepIntent`（由 `intent_router` 解出）翻译成 Tera 模板可消费的
//! `has_intent_hint` / `intent_label` / `intent_hint` 三个变量，让 4 个 react_*.toml
//! 模板能按意图精准嵌入提示，同时 MixedFocus 时零体积增量。
//!
//! 与 `intent_router` 的分工：
//! - `intent_router`：决定"是什么意图"（`StepIntent` 判定）
//! - `intent_handler`：决定"如何把意图呈现给 LLM"（文案 + 模板 flags）
//!
//! 模板端只需一段：
//! ```text
//! {% if has_intent_hint %}
//! 【{{ intent_label }}专属提示】
//! {{ intent_hint }}
//! {% endif %}
//! ```

pub mod hints;

pub use hints::IntentHandler;