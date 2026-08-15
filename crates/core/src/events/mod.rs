//! Events 模块
//!
//! Agent 执行事件系统。

pub mod chat_event;
pub mod event_types;
pub mod ui_action;

pub use chat_event::ChatEvent;
pub use ui_action::{
    FALLBACK_CONFIRM_ID, FALLBACK_CONFIRM_LABEL, MultiSelectOption, UIAction, UIActionType,
};

// 注意：内部事件类型不导出到 core 顶层
// 如需使用，通过完整路径访问：
//   planned_agent_core::events::event_types::ExecutionEvent
