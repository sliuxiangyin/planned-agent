//! Flexible Executor —— 灵活计划执行器。
//!
//! 把落库的灵活计划模板（`{ task, inputs, steps }`）连同运行参数跑起来，
//! 逐步执行并输出耗时 / token / 工具调用统计。
//!
//! 边界：不依赖 `agent-gui`，不依赖 `planner/`，只依赖 `core`（`AiClient`）
//! 与 `tool-manager`（`ToolRegistry`）。详细设计见
//! `docs/planned-agent/flexible-executor.md`。
//!
//! 目录结构（按「计划长什么样 / 怎么跑一次 / 宿主怎么驱动」三段分组）：
//! ```text
//! flexible/
//! ├── mod.rs              门面：声明子模块 + 对外导出（外面只 use 这里）
//! ├── plan/               计划期：落库模板的静态形态
//! │   ├── template.rs     模板强类型：task / inputs / steps
//! │   ├── output_schema.rs 输出契约：kind / goal / success / format / required / wanted
//! │   ├── placeholder.rs  ${name} 的收集 / 校验 / 替换
//! │   └── params.rs       运行参数表 + 步骤 intent / expected_output 的展开
//! ├── exec/               执行期：怎么跑一次
//! │   ├── executor/       总编排（mod / config / spill / resolve / prior / tools / logging）
//! │   ├── step/           单步执行（mod / llm / render）
//! │   ├── prompt.rs       内置提示词常量
//! │   ├── event.rs        进度事件 PlanRunEvent + PlanRunSink
//! │   ├── report.rs       执行报告 PlanRunReport / StepRunRecord
//! │   └── recipe.rs       工具链记忆：反参数化 + 从报告提炼配方
//! ├── run_service/        执行服务（宿主侧）：常驻循环 + 状态表 + 订阅
//! └── testing.rs          测试桩（#[cfg(test)]）
//! ```
//!
//! **对外路径不随目录重组改变**：下面 `pub use` 的名字一条未动，外面继续写
//! `planned_agent::flexible::{PlanStep, ExecutorConfig, ...}`。

mod exec;
mod plan;

// 执行服务自成一组类型（快照 / 命令 / 接缝 trait），故不做顶层 re-export，
// 对外路径统一为 `flexible::run_service::X`。
pub mod run_service;

#[cfg(test)]
mod testing;

pub use exec::event::{ChannelSink, NullSink, PlanRunEvent, PlanRunSink};
pub use exec::executor::{
    ExecutorConfig, FlexibleExecutor, DEFAULT_CACHE_DIR, DEFAULT_LLM_TIMEOUT_RETRIES,
    DEFAULT_LLM_TIMEOUT_SECS, DEFAULT_SPILL_PREVIEW_CHARS, DEFAULT_SPILL_THRESHOLD_CHARS,
};
pub use exec::recipe::{recipes_from_report, shape_arguments, StepToolRecipe, ToolCallShape};
pub use exec::report::{CallUsage, PlanRunReport, StepRunRecord, StepStatus, ToolCallRecord};
pub use plan::output_schema::{OutputKind, OutputSchema};
pub use plan::params::{render_step_expected_output, render_step_intent, PlanRunParams};
pub use plan::placeholder::{
    collect_from_schema, collect_from_steps, collect_placeholders, render, render_lenient, validate,
};
pub use plan::template::{FlexiblePlanTemplate, PlanInput, PlanStep};
