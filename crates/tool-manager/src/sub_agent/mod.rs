pub mod executor;
pub mod session;
pub mod stream;
pub mod types;

// 重新导出：接口定义来自 types，实现来自 executor
pub use types::{SubAgentRunOutcome, SubAgentSession, SubAgentSessionRunner};
pub use executor::{OneShotSubAgentRunner, SubAgentToolExecutor};
pub use session::SubAgentSessionStore;
pub use stream::{StreamKind, ToolStreamEvent, ToolStreamSender};