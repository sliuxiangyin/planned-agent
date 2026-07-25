//! `planned-agent-util` —— 通用工具函数 crate
//!
//! 与具体领域无关的小工具集合。任何 workspace crate 都可依赖此 crate。
//!
//! 当前成员：
//! - [`format`] —— 内容格式判断（HTML / Markdown / JSON / XML / CSV / Text）
//!
//! 新增工具：
//! - 在 `src/` 下新建 `xxx.rs`
//! - 在本 `lib.rs` 加 `pub mod xxx;`
//! - 调用方按需 `use planned_agent_util::xxx::Y;`

pub mod format;
