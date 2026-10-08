//! 执行器的内置提示词常量。
//!
//! 刻意不依赖 `prompt-manager` 的文件目录：宿主无需为执行器加载任何 prompt 文件。
//!
//! 按用途拆成子模块，避免一个文件里堆「所有提示词 + 各技能规范段」：
//!
//! - [`system`]：两个 system prompt 基准（单步 / 输出整理步）与单步 system prompt 的组装；
//! - [`environment`]：往 system prompt 尾部追加「运行环境」段；
//! - [`skills`]：各技能（`PlanCategory`）的**作业规范段** —— 增长热点，单独一处维护；
//! - [`task`]：单步 user 文本与输出契约文本的组装。
//!
//! **对外路径不变**：调用方照旧写 `exec::prompt::X`，本模块原样转出。

mod environment;
mod skills;
mod system;
mod task;

pub(crate) use skills::category_rule_section;
pub(crate) use system::{step_system_prompt, OUTPUT_RESOLVE_SYSTEM_PROMPT, STEP_SYSTEM_PROMPT};
pub(crate) use task::{build_output_contract_text, build_step_task};
