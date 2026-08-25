pub mod controller;
pub mod signals;
pub mod types;

pub use controller::{send_message, ensure_subscription, handle_user_action};
pub use signals::ChatSignals;
pub use types::{ChatMessage, PendingUI, ToolCallPhase, ToolViewData};
