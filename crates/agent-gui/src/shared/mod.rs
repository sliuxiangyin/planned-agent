//! 跨模块共享的 UI 状态类型。
//!
//! 存放不专属某个业务模块、被全局启动门（`crate::boot`）与各页面会话启动
//! （如灵活模式 `boot_flexible_session`）共同复用的「启动状态表达 + signal 写回」工具。

mod boot_phase;
mod boot_reporter;

pub use boot_phase::{BootPhase, OnProgress};
pub use boot_reporter::BootReporter;
