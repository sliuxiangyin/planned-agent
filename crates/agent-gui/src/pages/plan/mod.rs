//! Plan 页面模块入口。
//!
//! 内部结构：
//! - `types` — 内部类型与 `Message` 辅助函数
//! - `page` — `PlanPage` 组件（对外暴露）
//! - `chat` — 聊天流处理（send / run / finalize / user action）
//!
//! 仅 `PlanPage` 通过 `pub use` 对外暴露，保持内部 API 收敛。

mod chat;
mod page;
mod types;

pub use page::PlanPage;
