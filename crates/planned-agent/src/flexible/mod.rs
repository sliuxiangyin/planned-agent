//! Flexible Executor —— 灵活计划执行器。
//!
//! 把落库的灵活计划模板（`{ task, inputs, steps }`）连同运行参数跑起来，
//! 逐步执行并输出耗时 / token / 工具调用统计。
//!
//! 边界：不依赖 `agent-gui`，不依赖 `planner/`，只依赖 `core`（`AiClient`）
//! 与 `tool-manager`（`ToolRegistry`）。详细设计见
//! `docs/planned-agent/flexible-executor.md`。
//!
//! 目录结构：
//! ```text
//! flexible/
//! ├── mod.rs          门面：声明子模块 + 对外导出（外面只 use 这里）
//! ├── template.rs     模板强类型：task / inputs / steps
//! ├── output_schema.rs 输出契约：kind / goal / success / format / required / wanted
//! ├── placeholder.rs  ${name} 的收集 / 校验 / 替换
//! ├── params.rs       运行参数表 + 步骤 intent 的展开
//! ├── event.rs        进度事件 PlanRunEvent + PlanRunSink
//! ├── report.rs       执行报告 PlanRunReport / StepRunRecord
//! ├── prompt.rs       内置提示词常量
//! ├── step.rs         单步执行：工具循环 + 耗时 / token
//! ├── executor.rs     总编排
//! ├── run_service/    执行服务（宿主侧）：常驻循环 + 状态表 + 订阅
//! └── testing.rs      测试桩（#[cfg(test)]）
//! ```

mod event;
mod executor;
mod output_schema;
mod params;
mod placeholder;
mod prompt;
mod report;
mod step;
mod template;

// 执行服务自成一组类型（快照 / 命令 / 接缝 trait），故不做顶层 re-export，
// 对外路径统一为 `flexible::run_service::X`。
pub mod run_service;

#[cfg(test)]
mod testing;

pub use event::{ChannelSink, NullSink, PlanRunEvent, PlanRunSink};
pub use executor::{ExecutorConfig, FlexibleExecutor};
pub use output_schema::{OutputKind, OutputSchema};
pub use params::{render_step_intent, PlanRunParams};
pub use placeholder::{
    collect_from_schema, collect_from_steps, collect_placeholders, render, render_lenient, validate,
};
pub use report::{CallUsage, PlanRunReport, StepRunRecord, StepStatus};
pub use template::{FlexiblePlanTemplate, PlanInput, PlanStep};
