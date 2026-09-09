pub mod controller;
pub mod signals;
pub mod signals_history;
pub mod signals_pending;
pub mod signals_status;
pub mod signals_streaming;
pub mod signals_tool;
pub mod signals_turn;
pub mod types;

// 新单一订阅桥（阶段 1–3 并存，尚未接线到组件）
pub mod bridge;
pub mod reduce;
pub mod view;

pub use controller::{ensure_subscription, handle_user_action, send_message};
pub use signals::ChatSignals;
pub use types::{AgentEvent, AgentViewData, Bubble, PendingUI, ToolCallPhase, ToolViewData};
