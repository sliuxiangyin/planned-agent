//! 服务层：基于 context 派生的服务 hooks
//!
//! 与 context/ 的区别：
//! - context/*：全局初始化时注入的"原料"（AiContext / PromptContext / ...）
//! - services/*：把多个 context 组合成的高级服务（PlannerService / ...）
//!
//! 调用方（pages/）只使用服务实例，不必关心服务内部依赖哪些 context。

pub mod planner_service;