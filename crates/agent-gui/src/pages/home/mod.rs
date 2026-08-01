//! 指挥中心（Command Center）页面模块入口。
//!
//! 内部结构：
//! - `types` — PlanMeta、AgentInsight、TimelineEntry 等数据模型
//! - `page` — `HomePage` 组件（对外暴露）
//! - `components` — 子组件（ai_core、active_plans、agent_insights、quick_actions、timeline）

mod components;
mod page;
mod types;

pub use page::HomePage;
pub use page::PageRoute;
