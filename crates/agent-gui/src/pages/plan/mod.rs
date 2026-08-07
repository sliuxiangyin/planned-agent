//! Plan 页面模块入口。
//!
//! 内部结构：
//! - `types` — 内部类型与 `Message` 辅助函数
//! - `page` — `PlanPage` 组件（对外暴露）
//! - `chat` — 聊天流处理（周密模式）
//! - `message` — 消息列表渲染（周密模式）
//! - `components` — 共享 UI 组件
//! - `shared` — 共享辅助模块（加载/保存）
//! - `flexible` — 灵活模式工作流组件
//! - `left_panel` — 左侧面板（顶栏 + Bento 瓷块 + 删除弹窗）
//!
//! 仅 `PlanPage` 通过 `pub use` 对外暴露，保持内部 API 收敛。

mod chat;
mod components;
mod flexible;
mod left_panel;
mod message;
mod page;
mod shared;
mod types;

pub use page::PlanPage;
