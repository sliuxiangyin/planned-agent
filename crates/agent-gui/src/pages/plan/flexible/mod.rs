//! 灵活模式组件模块。
//!
//! - `workflow` — FlexibleWorkflow 状态机编排 + 三段布局
//! - `context_header` — 历史上下文折叠区
//! - `requirement_input` — 固定底部需求输入区
//! - `execution_view` — 中间执行步骤展示

pub mod context_header;
pub mod execution_view;
pub mod requirement_input;
pub mod workflow;
