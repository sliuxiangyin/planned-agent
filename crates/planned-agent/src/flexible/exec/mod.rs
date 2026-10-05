//! 执行期：**怎么跑一次**灵活计划。
//!
//! [`executor`] 负责总编排（解析模板 → 逐步执行 → 整理产出），[`step`] 负责
//! 单步的工具循环，[`event`] / [`report`] 是它对外的两条输出通道（进度与报告）。

pub mod event;
pub mod executor;
pub mod recipe;
pub mod report;
pub mod step;

mod prompt;
